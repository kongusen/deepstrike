import type { DoneEvent, StreamEvent, TextDelta, WorkflowNodesSubmittedEvent } from "../types.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, LoopResult, SubAgentResult, TerminationReason,
  KernelAgentRole, WorkflowNodeSpec,
} from "./types/agent.js"
import { agentRunSpecToKernel, findSpawnProcessObservation, spawnObservationToManifest } from "./types/agent.js"
import type { RuntimeOptions } from "./runner.js"
import type { SessionEvent, SessionLog } from "./session-log.js"
import { FilteredExecutionPlane } from "./filtered-plane.js"
import { kernelApply, type KernelObservation } from "./kernel-step.js"

export interface SubAgentRunContext {
  parentOpts: RuntimeOptions
  parentSessionId: string
  spec: AgentRunSpec
  manifest: AgentProcessChangedObservation
  sessionLog: SessionLog
  /** M5 v2.1: set when this child is a workflow node — propagated so a nested `start_workflow`
   *  FLATTENS to the parent kernel rather than auto-pivoting into its own bootstrap. */
  isWorkflowNode?: boolean
  /** W-N1 tool exposure. The kernel omits an EMPTY `permitted_capability_ids` on the wire, so a
   *  grant-less workflow node is indistinguishable from a zero-cap spawn at the manifest — the
   *  caller states intent instead: `"inherit"` = run on the parent's execution plane with its
   *  meta-tool availability (trusted workflow nodes — they carried no grant list by design, and
   *  filtering on the missing list ran every DAG node TOOL-LESS); `"filtered"` (default) = filter
   *  to the manifest grants, empty ⇒ deny-all (spawn path, quarantined nodes). */
  toolAccess?: "inherit" | "filtered"
  /** #2-B-ii: parent-controlled abort — when the kernel preempts this node (`AgentPreempted`), the
   *  orchestrator interrupts the child runner, cancelling its in-flight LLM call. */
  abortSignal?: AbortSignal
}

/** #2-B-ii: bridge a parent AbortSignal to a child runner's `interrupt()` (fires now if already aborted). */
function linkAbort(signal: AbortSignal | undefined, runner: { interrupt(): void }): void {
  if (!signal) return
  if (signal.aborted) { runner.interrupt(); return }
  signal.addEventListener("abort", () => runner.interrupt(), { once: true })
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
    normalized === "milestone_exceeded" ||
    normalized === "context_overflow" ||
    normalized === "no_progress"
  ) {
    return normalized as TerminationReason
  }
  return status
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

/** Derive which meta-tools a child runner should expose based on permitted IDs and available sources. */
function deriveMetaTools(permitted: Set<string>, opts: RuntimeOptions): Set<string> {
  const metaTools = new Set<string>()
  if (permitted.has("skill") && opts.skillContentMap?.size) metaTools.add("skill")
  if (permitted.has("memory") && opts.dreamStore) metaTools.add("memory")
  if (permitted.has("knowledge") && opts.knowledgeSource) metaTools.add("knowledge")
  if (permitted.has("update_plan") && opts.enablePlanTool) metaTools.add("update_plan")
  return metaTools
}

/** W-N1: meta-tools by source availability alone — a `toolAccess: "inherit"` child gets the same
 *  meta surface a top-level run of these options would. */
function availableMetaTools(opts: RuntimeOptions): Set<string> {
  const metaTools = new Set<string>()
  if (opts.skillContentMap?.size) metaTools.add("skill")
  if (opts.dreamStore) metaTools.add("memory")
  if (opts.knowledgeSource) metaTools.add("knowledge")
  if (opts.enablePlanTool) metaTools.add("update_plan")
  return metaTools
}

/** Host-side driver for kernel-isolated sub-agent runs. */
export class SubAgentOrchestrator {
  async run(ctx: SubAgentRunContext): Promise<SubAgentResult> {
    // W-N1: "inherit" runs the child on the parent's plane with availability-derived meta-tools
    // (trusted workflow nodes); "filtered" (default) filters to the manifest grants, empty ⇒
    // deny-all (spawn path, quarantined nodes).
    const inherit = ctx.toolAccess === "inherit"
    const permitted = new Set(ctx.manifest.permitted_capability_ids ?? [])
    const metaTools = inherit ? availableMetaTools(ctx.parentOpts) : deriveMetaTools(permitted, ctx.parentOpts)
    const execPlane = inherit
      ? ctx.parentOpts.executionPlane
      : new FilteredExecutionPlane(ctx.parentOpts.executionPlane, permitted, metaTools)

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
      // O3: per-child turn / wall-clock caps (fall back to the inherited limits).
      maxTurns: ctx.spec.maxTurns ?? ctx.parentOpts.maxTurns,
      timeoutMs: ctx.spec.maxWallMs ?? ctx.parentOpts.timeoutMs,
      executionPlane: execPlane,
      agentId: ctx.spec.identity.agentId,
      systemPrompt,
      sessionLog: ctx.sessionLog,
      skillContentMap: metaTools.has("skill") ? ctx.parentOpts.skillContentMap : undefined,
      dreamStore: metaTools.has("memory") ? ctx.parentOpts.dreamStore : undefined,
      knowledgeSource: metaTools.has("knowledge") ? ctx.parentOpts.knowledgeSource : undefined,
      enablePlanTool: metaTools.has("update_plan") ? ctx.parentOpts.enablePlanTool : undefined,
      // M5 v2.1: a workflow node's `start_workflow` flattens to the parent kernel (no nested pivot).
      isWorkflowNode: ctx.isWorkflowNode,
      // The child runs under ITS OWN spec, never the parent's: the spread above would otherwise
      // leak the parent's `runSpec` (identity, capability filter — and an armed `loopRound`,
      // giving every child a phantom pace tool). A loop-node iteration carries its own minimal
      // spec to arm the pacing trap (DW-3); everything else runs spec-less as before.
      runSpec: ctx.spec.loopRound
        ? { identity: ctx.spec.identity, role: ctx.spec.role, goal: ctx.spec.goal, loopRound: ctx.spec.loopRound }
        : undefined,
    })
    // #2-B-ii: parent preempt → interrupt the child (cancels its in-flight LLM call).
    linkAbort(ctx.abortSignal, childRunner)

    let done: DoneEvent | undefined
    let finalText = ""
    // R3-1: collect any nodes this node's agent submitted via the `submit_workflow_nodes` tool.
    const submittedNodes: WorkflowNodeSpec[] = []
    for await (const evt of childRunner.run({
      sessionId: ctx.spec.identity.sessionId,
      goal: ctx.spec.goal,
    })) {
      if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
      if (evt.type === "done") done = evt as DoneEvent
      if (evt.type === "workflow_nodes_submitted") submittedNodes.push(...(evt as WorkflowNodesSubmittedEvent).nodes)
    }

    const loopResult: LoopResult = {
      termination: terminationFromStatus(done?.status ?? "error"),
      turnsUsed: done?.iterations ?? 0,
      totalTokensUsed: done?.totalTokens ?? 0,
      ...(finalText
        ? { finalMessage: { role: "assistant", content: finalText, toolCalls: [] } }
        : {}),
      // DW-3: surface the kernel-adjudicated pace decision (loop-node iterations consume it as the
      // continuation vocabulary). SDK-internal; stripped by subAgentResultToKernel.
      ...(done?.paceDecision ? { paceDecision: done.paceDecision } : {}),
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
  const kernel = await (await import("./kernel.js")).getKernel()
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
