import type { DoneEvent, StreamEvent, TextDelta, WorkflowNodesSubmittedEvent } from "../types.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, LoopResult, SubAgentResult, TerminationReason,
  KernelAgentRole, WorkflowNodeSpec,
} from "../types/agent.js"
import { agentRunSpecToKernel, findSpawnProcessObservation, spawnObservationToManifest } from "../types/agent.js"
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
  harness?: {
    evalProvider: import("../types.js").LLMProvider
    maxAttempts?: number
  }
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
      executionPlane: filteredPlane,
      agentId: ctx.spec.identity.agentId,
      systemPrompt,
      sessionLog: ctx.sessionLog,
      skillDir: metaTools.has("skill") ? ctx.parentOpts.skillDir : undefined,
      dreamStore: metaTools.has("memory") ? ctx.parentOpts.dreamStore : undefined,
      knowledgeSource: metaTools.has("knowledge") ? ctx.parentOpts.knowledgeSource : undefined,
      enablePlanTool: metaTools.has("update_plan") ? ctx.parentOpts.enablePlanTool : undefined,
    })

    yield* childRunner.run({
      sessionId: ctx.spec.identity.sessionId,
      goal: ctx.spec.goal,
      inheritEvents,
    })
  }

  async run(ctx: SubAgentRunContext): Promise<SubAgentResult> {
    if (ctx.harness) {
      const { RuntimeRunner } = await import("./runner.js")
      const { HarnessLoop } = await import("../harness/harness.js")
      const permitted = new Set(ctx.manifest.permitted_capability_ids ?? [])
      const metaTools = deriveMetaTools(permitted, ctx.parentOpts)
      const filteredPlane = new FilteredExecutionPlane(ctx.parentOpts.executionPlane, permitted, metaTools)
      const childRunner = new RuntimeRunner({
        ...ctx.parentOpts,
        executionPlane: filteredPlane,
        agentId: ctx.spec.identity.agentId,
        sessionLog: ctx.sessionLog,
        skillDir: metaTools.has("skill") ? ctx.parentOpts.skillDir : undefined,
        dreamStore: metaTools.has("memory") ? ctx.parentOpts.dreamStore : undefined,
        knowledgeSource: metaTools.has("knowledge") ? ctx.parentOpts.knowledgeSource : undefined,
        enablePlanTool: metaTools.has("update_plan") ? ctx.parentOpts.enablePlanTool : undefined,
      })
      const loop = new HarnessLoop(childRunner, ctx.harness.evalProvider, {
        maxAttempts: ctx.harness.maxAttempts ?? 3,
      })
      const outcome = await loop.run({
        goal: ctx.spec.goal,
        criteria: (ctx.spec.milestones?.phases.flatMap(p => p.criteria) ?? [])
          .filter((t): t is string => typeof t === "string")
          .map(text => ({ text, required: true })),
      })
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
