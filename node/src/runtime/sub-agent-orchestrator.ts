import type { DoneEvent, StreamEvent, TextDelta, WorkflowNodesSubmittedEvent } from "../types.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, LoopResult, SubAgentResult, TerminationReason,
  KernelAgentRole, WorkflowNodeSpec,
} from "../types/agent.js"
import { agentRunSpecToKernel, findSpawnProcessObservation, spawnObservationToManifest } from "../types/agent.js"
import type { RuntimeOptions } from "./runner.js"
import type { SessionEvent, SessionLog } from "./session-log.js"
import { FilteredExecutionPlane } from "./filtered-plane.js"
import { WorktreeExecutionPlane } from "./worktree-plane.js"
import type { ExecutionPlane } from "./execution-plane.js"
import { kernelApply, type KernelObservation } from "./kernel-step.js"

export interface SubAgentRunContext {
  parentOpts: RuntimeOptions
  parentSessionId: string
  spec: AgentRunSpec
  manifest: AgentProcessChangedObservation
  sessionLog: SessionLog
  harness?: {
    evalProvider: import("../types.js").LLMProvider
    maxAttempts?: number
  }
  /** M5 v2.1: set when this child is a workflow node (spawned by the workflow driver). Propagated to
   *  the child runner so a nested `start_workflow` FLATTENS to the parent kernel rather than
   *  auto-pivoting into its own bootstrap (which would fragment the one-kernel/one-quota governance). */
  isWorkflowNode?: boolean
  /** #2-B-ii: parent-controlled abort. When this fires (the kernel preempted this node via
   *  `InterruptNow` → `AgentPreempted`), the orchestrator interrupts the child runner, cancelling its
   *  in-flight LLM call. */
  abortSignal?: AbortSignal
}

function terminationFromStatus(status: string): TerminationReason | string {
  const normalized = status.toLowerCase()
  if (
    normalized === "completed" ||
    normalized === "max_turns" ||
    normalized === "token_budget" ||
    normalized === "timeout" ||
    normalized === "user_abort" ||
    normalized === "error" ||
    normalized === "milestone_exceeded"
  ) {
    return normalized as TerminationReason
  }
  return status
}

/** M3/G4: if this sub-agent is an `isolation: "worktree"` node and a worktree manager is configured,
 *  wrap its plane in a `WorktreeExecutionPlane` (creates a git worktree, injects it as `cwd`, removes
 *  it on cleanup). Returns the plane to use plus a cleanup hook the caller must run when the sub-agent
 *  finishes. Without a manager (or for non-worktree nodes) this is a pass-through with a no-op cleanup. */
function withWorktree(
  ctx: SubAgentRunContext,
  plane: ExecutionPlane,
): { plane: ExecutionPlane; cleanup: () => Promise<void> } {
  if (ctx.manifest.isolation === "worktree" && ctx.parentOpts.worktreeManager) {
    const wt = new WorktreeExecutionPlane(plane, ctx.parentOpts.worktreeManager, ctx.spec.identity.agentId)
    return { plane: wt, cleanup: () => wt.cleanup() }
  }
  return { plane, cleanup: async () => {} }
}

/** M1/G3 intelligence routing: resolve the provider for a sub-agent from its spec's `modelHint`.
 *  Falls back to the parent provider when there is no hint or no `providerFor` hook resolves it. */
export function resolveProvider(opts: RuntimeOptions, modelHint?: string): RuntimeOptions["provider"] {
  if (modelHint && opts.providerFor) {
    const routed = opts.providerFor(modelHint)
    if (routed) return routed
  }
  return opts.provider
}

/** #2-B-ii: bridge a parent-controlled AbortSignal to a child runner's `interrupt()` — fires now if
 *  the signal is already aborted (creation race), else once when it aborts. */
function linkAbort(signal: AbortSignal | undefined, runner: { interrupt(): void }): void {
  if (!signal) return
  if (signal.aborted) { runner.interrupt(); return }
  signal.addEventListener("abort", () => runner.interrupt(), { once: true })
}

/** Derive which meta-tools a child runner should expose based on permitted IDs and available sources. */
function deriveMetaTools(permitted: Set<string>, opts: RuntimeOptions): Set<string> {
  const metaTools = new Set<string>()
  if (permitted.has("skill") && opts.skillDir) metaTools.add("skill")
  if (permitted.has("memory") && opts.dreamStore) metaTools.add("memory")
  if (permitted.has("knowledge") && opts.knowledgeSource) metaTools.add("knowledge")
  if (permitted.has("update_plan") && opts.enablePlanTool) metaTools.add("update_plan")
  return metaTools
}

/** Host-side driver for kernel-isolated sub-agent runs. */
export class SubAgentOrchestrator {
  async *stream(ctx: SubAgentRunContext): AsyncIterable<StreamEvent> {
    const permitted = new Set(ctx.manifest.permitted_capability_ids ?? [])
    const metaTools = deriveMetaTools(permitted, ctx.parentOpts)
    const filteredPlane = new FilteredExecutionPlane(ctx.parentOpts.executionPlane, permitted, metaTools)
    // M3/G4: a worktree node runs inside its own git worktree (created here, removed in `finally`).
    const { plane: execPlane, cleanup: cleanupWorktree } = withWorktree(ctx, filteredPlane)

    let systemPrompt = ctx.parentOpts.systemPrompt
    let inheritEvents: Array<{ seq: number; event: SessionEvent }> | undefined

    if (ctx.manifest.context_inheritance === "full") {
      inheritEvents = await ctx.sessionLog.read(ctx.parentSessionId)
    } else if (ctx.manifest.context_inheritance === "system_only") {
      const parentEvents = await ctx.sessionLog.read(ctx.parentSessionId)
      const started = parentEvents.find(e => e.event.kind === "run_started")
      if (started?.event.kind === "run_started" && started.event.system_prompt) {
        systemPrompt = started.event.system_prompt
      }
    }

    const { RuntimeRunner } = await import("./runner.js")
    const childRunner = new RuntimeRunner({
      ...ctx.parentOpts,
      // M1/G3: route to the node's hinted model (falls back to the parent provider).
      provider: resolveProvider(ctx.parentOpts, ctx.spec.modelHint),
      // M4/G5: cap the child run at the node's token budget (falls back to the inherited cap).
      maxTotalTokens: ctx.spec.tokenBudget ?? ctx.parentOpts.maxTotalTokens,
      executionPlane: execPlane,
      agentId: ctx.spec.identity.agentId,
      systemPrompt,
      sessionLog: ctx.sessionLog,
      skillDir: metaTools.has("skill") ? ctx.parentOpts.skillDir : undefined,
      dreamStore: metaTools.has("memory") ? ctx.parentOpts.dreamStore : undefined,
      knowledgeSource: metaTools.has("knowledge") ? ctx.parentOpts.knowledgeSource : undefined,
      enablePlanTool: metaTools.has("update_plan") ? ctx.parentOpts.enablePlanTool : undefined,
      // M5 v2.1: a workflow node's `start_workflow` flattens to the parent kernel (no nested pivot).
      isWorkflowNode: ctx.isWorkflowNode,
    })
    // #2-B-ii: when the parent preempts this node (kernel `AgentPreempted`), interrupt the child —
    // cancelling its in-flight LLM call. Handle an already-aborted signal too (creation race).
    linkAbort(ctx.abortSignal, childRunner)

    try {
      yield* childRunner.run({
        sessionId: ctx.spec.identity.sessionId,
        goal: ctx.spec.goal,
        inheritEvents,
      })
    } finally {
      await cleanupWorktree()
    }
  }

  async run(ctx: SubAgentRunContext): Promise<SubAgentResult> {
    if (ctx.harness) {
      const { RuntimeRunner } = await import("./runner.js")
      const { HarnessLoop } = await import("../harness/harness.js")
      const permitted = new Set(ctx.manifest.permitted_capability_ids ?? [])
      const metaTools = deriveMetaTools(permitted, ctx.parentOpts)
      const filteredPlane = new FilteredExecutionPlane(ctx.parentOpts.executionPlane, permitted, metaTools)
      // M3/G4: worktree isolation for a worktree node (cleaned up in `finally` below).
      const { plane: execPlane, cleanup: cleanupWorktree } = withWorktree(ctx, filteredPlane)
      const childRunner = new RuntimeRunner({
        ...ctx.parentOpts,
        // M1/G3: route to the node's hinted model (falls back to the parent provider).
        provider: resolveProvider(ctx.parentOpts, ctx.spec.modelHint),
        // M4/G5: cap the child run at the node's token budget (falls back to the inherited cap).
        maxTotalTokens: ctx.spec.tokenBudget ?? ctx.parentOpts.maxTotalTokens,
        executionPlane: execPlane,
        agentId: ctx.spec.identity.agentId,
        sessionLog: ctx.sessionLog,
        skillDir: metaTools.has("skill") ? ctx.parentOpts.skillDir : undefined,
        dreamStore: metaTools.has("memory") ? ctx.parentOpts.dreamStore : undefined,
        knowledgeSource: metaTools.has("knowledge") ? ctx.parentOpts.knowledgeSource : undefined,
        enablePlanTool: metaTools.has("update_plan") ? ctx.parentOpts.enablePlanTool : undefined,
        // M5 v2.1: a workflow node's `start_workflow` flattens to the parent kernel (no nested pivot).
        isWorkflowNode: ctx.isWorkflowNode,
      })
      // #2-B-ii: parent preempt → interrupt the child (cancels its in-flight LLM call).
      linkAbort(ctx.abortSignal, childRunner)
      const loop = new HarnessLoop(childRunner, ctx.harness.evalProvider, {
        maxAttempts: ctx.harness.maxAttempts ?? 3,
      })
      let outcome
      try {
        outcome = await loop.run({
          goal: ctx.spec.goal,
          criteria: (ctx.spec.milestones?.phases.flatMap(p => p.criteria) ?? [])
            .filter((t): t is string => typeof t === "string")
            .map(text => ({ text, required: true })),
        })
      } finally {
        await cleanupWorktree()
      }
      return {
        agentId: ctx.spec.identity.agentId,
        result: {
          termination: outcome.passed ? "completed" : "error",
          turnsUsed: outcome.iterations,
          totalTokensUsed: outcome.totalTokens,
          ...(outcome.result ? { finalMessage: { role: "assistant" as const, content: outcome.result, toolCalls: [] } } : {}),
        },
        // R3-1: surface nodes the agent submitted under the harness so `runWorkflow` appends them.
        ...(outcome.submittedNodes?.length ? { submittedNodes: outcome.submittedNodes } : {}),
      }
    }

    let done: DoneEvent | undefined
    let finalText = ""
    // R3-1: collect any nodes this node's agent submitted via the `submit_workflow_nodes` tool (the
    // runner surfaces them as `workflow_nodes_submitted` because the workflow lives in the parent
    // kernel, not this child's). `runWorkflow` sends them to the parent kernel.
    const submittedNodes: WorkflowNodeSpec[] = []
    for await (const evt of this.stream(ctx)) {
      if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
      if (evt.type === "done") done = evt as DoneEvent
      if (evt.type === "workflow_nodes_submitted") {
        submittedNodes.push(...(evt as WorkflowNodesSubmittedEvent).nodes)
      }
    }
    const loopResult: LoopResult = {
      termination: terminationFromStatus(done?.status ?? "error"),
      turnsUsed: done?.iterations ?? 0,
      totalTokensUsed: done?.totalTokens ?? 0,
      ...(finalText ? { finalMessage: { role: "assistant", content: finalText, toolCalls: [] } } : {}),
    }
    return {
      agentId: ctx.spec.identity.agentId,
      result: loopResult,
      ...(submittedNodes.length ? { submittedNodes } : {}),
    }
  }
}

export const defaultSubAgentOrchestrator = new SubAgentOrchestrator()

/** Kernel spawn without an active parent run loop (harness / coordinator use). */
export async function spawnStandalone(
  parentOpts: RuntimeOptions,
  parentSessionId: string,
  spec: AgentRunSpec,
  orchestrator: SubAgentOrchestrator = defaultSubAgentOrchestrator,
): Promise<SubAgentResult> {
  const kernel = (await import("../kernel.js")).getKernel()
  const runtime = new kernel.KernelRuntime({
    maxTokens: parentOpts.maxTokens,
    maxTurns: parentOpts.maxTurns ?? 25,
    timeoutMs: parentOpts.timeoutMs !== undefined ? BigInt(parentOpts.timeoutMs) : undefined,
  })
  const pending: KernelObservation[] = []

  kernelApply(runtime, pending, { kind: "start_run", task: { goal: "coordinator", criteria: [] } })
  const observations = kernelApply(runtime, pending, {
    kind: "spawn_sub_agent",
    spec: agentRunSpecToKernel(spec),
    parent_session_id: parentSessionId,
  })

  const spawned = findSpawnProcessObservation(observations)
  if (!spawned) {
    throw new Error("spawn_sub_agent did not emit agent_process_changed")
  }

  const manifest = spawnObservationToManifest(spawned, spec, parentSessionId)
  await parentOpts.sessionLog.append(parentSessionId, {
    kind: "agent_process_changed",
    turn: manifest.turn ?? 0,
    agent_id: manifest.agent_id,
    parent_session_id: manifest.parent_session_id,
    role: manifest.role,
    isolation: manifest.isolation,
    context_inheritance: manifest.context_inheritance,
    state: "running",
    permitted_capability_ids: manifest.permitted_capability_ids ?? [],
  })

  return orchestrator.run({
    parentOpts,
    parentSessionId,
    spec,
    manifest,
    sessionLog: parentOpts.sessionLog,
  })
}
