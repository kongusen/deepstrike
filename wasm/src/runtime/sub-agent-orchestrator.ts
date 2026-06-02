import type { DoneEvent, StreamEvent, TextDelta } from "../types.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, LoopResult, SubAgentResult, TerminationReason,
  KernelAgentRole,
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

/** Host-side driver for kernel-isolated sub-agent runs. */
export class SubAgentOrchestrator {
  async run(ctx: SubAgentRunContext): Promise<SubAgentResult> {
    const permitted = new Set(ctx.manifest.permitted_capability_ids ?? [])
    const filteredPlane = new FilteredExecutionPlane(ctx.parentOpts.executionPlane, permitted)

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
    })

    let done: DoneEvent | undefined
    let finalText = ""
    for await (const evt of childRunner.run({
      sessionId: ctx.spec.identity.sessionId,
      goal: ctx.spec.goal,
    })) {
      if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
      if (evt.type === "done") done = evt as DoneEvent
    }

    const loopResult: LoopResult = {
      termination: terminationFromStatus(done?.status ?? "error"),
      turnsUsed: done?.iterations ?? 0,
      totalTokensUsed: done?.totalTokens ?? 0,
      ...(finalText
        ? { finalMessage: { role: "assistant", content: finalText, toolCalls: [] } }
        : {}),
    }

    return { agentId: ctx.spec.identity.agentId, result: loopResult }
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
