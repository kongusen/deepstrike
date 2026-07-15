import type { KernelRuntimeInstance } from "../../src/kernel.js"
import type { SessionLog } from "../../src/runtime/session-log.js"
import { durableKernelStep, kernelStep } from "../../src/runtime/kernel-step.js"

type RawStep = {
  version: number
  actions: Array<Record<string, unknown>>
  observations: Array<Record<string, unknown>>
  faults?: Array<Record<string, unknown>>
}

const pendingEffects = new WeakMap<KernelRuntimeInstance, Map<string, string>>()

const resultToEffect = new Map([
  ["provider_result", "call_provider"],
  ["provider_error", "call_provider"],
  ["tool_results", "execute_tool"],
  ["milestone_result", "evaluate_milestone"],
  ["approval_result", "request_approval"],
  ["workflow_spawn_result", "spawn_workflow"],
  ["preempt_result", "preempt_sub_agents"],
  ["memory_persist_result", "persist_memory"],
  ["memory_query_result", "query_memory"],
  ["large_result_spool_result", "spool_large_result"],
  ["page_out_archive_result", "archive_page_out"],
])

export function stepKernelV2<T = {
  version: number
  actions: Array<Record<string, unknown>>
  observations: Array<Record<string, unknown>>
  faults?: Array<Record<string, unknown>>
}>(runtime: KernelRuntimeInstance, event: Record<string, unknown>): T {
  const effects = pendingEffects.get(runtime) ?? new Map<string, string>()
  pendingEffects.set(runtime, effects)
  const effectKind = resultToEffect.get(String(event.kind))
  const correlated = effectKind && event.effect_id === undefined
    ? { ...event, effect_id: effects.get(effectKind) }
    : event
  const step = kernelStep(runtime, correlated) as RawStep
  for (const action of step.actions) {
    if (typeof action.kind === "string" && typeof action.effect_id === "string") {
      effects.set(action.kind, action.effect_id)
    }
  }
  return step as T
}

/** Test host that commits effects whose success needs no external fixture data. */
export function stepKernelV2WithHostEffects(runtime: KernelRuntimeInstance, event: Record<string, unknown>): RawStep {
  const first = stepKernelV2<RawStep>(runtime, event)
  const action = first.actions[0]
  if (!action) return first

  let result: Record<string, unknown> | undefined
  if (action.kind === "spawn_workflow") {
    const nodes = (action.nodes as Array<Record<string, unknown>>) ?? []
    result = {
      kind: "workflow_spawn_result",
      effect_id: action.effect_id,
      started_agent_ids: nodes.map(node => String(node.agent_id ?? "")),
      failures: [],
    }
  } else if (action.kind === "persist_memory") {
    result = { kind: "memory_persist_result", effect_id: action.effect_id }
  } else if (action.kind === "query_memory") {
    result = { kind: "memory_query_result", effect_id: action.effect_id, hits: [] }
  }
  if (!result) return first

  const committed = stepKernelV2<RawStep>(runtime, result)
  return {
    ...committed,
    observations: [...first.observations, ...committed.observations],
  }
}

export function startKernelV2(runtime: KernelRuntimeInstance, goal = "parent"): void {
  const step = stepKernelV2(runtime, { kind: "start_run", task: { goal, criteria: [] } })
  if (step.faults?.length) throw new Error(JSON.stringify(step.faults[0]))
}

/**
 * Durable counterpart for runner-integration tests. A runtime must enter the transaction log on
 * its very first transition; seeding it through `step()` and attaching it to RuntimeRunner later
 * would create a journal whose first committed transaction starts at step_seq > 1.
 */
export async function durableStepKernelV2<T = RawStep>(
  runtime: KernelRuntimeInstance,
  sessionLog: SessionLog,
  sessionId: string,
  event: Record<string, unknown>,
): Promise<T> {
  const effects = pendingEffects.get(runtime) ?? new Map<string, string>()
  pendingEffects.set(runtime, effects)
  const effectKind = resultToEffect.get(String(event.kind))
  const correlated = effectKind && event.effect_id === undefined
    ? { ...event, effect_id: effects.get(effectKind) }
    : event
  const step = await durableKernelStep(runtime, sessionLog, sessionId, correlated) as RawStep
  for (const action of step.actions) {
    if (typeof action.kind === "string" && typeof action.effect_id === "string") {
      effects.set(action.kind, action.effect_id)
    }
  }
  return step as T
}

export async function durableStartKernelV2(
  runtime: KernelRuntimeInstance,
  sessionLog: SessionLog,
  sessionId: string,
  goal = "parent",
): Promise<void> {
  const step = await durableStepKernelV2(runtime, sessionLog, sessionId, {
    kind: "start_run",
    task: { goal, criteria: [] },
  })
  if (step.faults?.length) throw new Error(JSON.stringify(step.faults[0]))
}
