/** Adapt scripted event-kernel fakes to the canonical runtime surface used by workflow tests. */
import type { KernelRunnerAction } from "../../src/runtime/kernel-step.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"

type ScriptedKernel = {
  step(inputJson: string): string
}

function asObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? value as Record<string, unknown> : {}
}

function mapAction(raw: Record<string, unknown>): KernelRunnerAction | null {
  const kind = String(raw.kind ?? "")
  const effectId = String(raw.effect_id ?? "")
  switch (kind) {
    case "call_provider":
      return {
        kind: "call_provider",
        effectId,
        context: asObject(raw.context) as never,
        tools: (Array.isArray(raw.tools) ? raw.tools : []) as never,
      }
    case "execute_tool":
    case "execute_tools":
      return {
        kind: "execute_tool",
        effectId,
        calls: (Array.isArray(raw.calls) ? raw.calls : []).map(value => {
          const call = asObject(value)
          return {
            id: String(call.id ?? call.call_id ?? ""),
            name: String(call.name ?? ""),
            arguments: typeof call.arguments === "string"
              ? call.arguments
              : JSON.stringify(call.arguments ?? {}),
          }
        }),
      }
    case "spawn_workflow":
      return {
        kind: "spawn_workflow",
        effectId,
        nodes: (Array.isArray(raw.nodes) ? raw.nodes : []) as Array<Record<string, unknown>>,
        ...(raw.budget ? { budget: asObject(raw.budget) } : {}),
      }
    case "preempt_sub_agents":
      return {
        kind: "preempt_sub_agents",
        effectId,
        agentIds: (Array.isArray(raw.agentIds) ? raw.agentIds : Array.isArray(raw.agent_ids) ? raw.agent_ids : [])
          .map(String),
        reason: String(raw.reason ?? ""),
      }
    case "evaluate_milestone":
      return {
        kind: "evaluate_milestone",
        effectId,
        phaseId: String(raw.phase_id ?? raw.phaseId ?? ""),
        criteria: Array.isArray(raw.criteria) ? raw.criteria.map(String) : [],
        requiredEvidence: Array.isArray(raw.required_evidence)
          ? raw.required_evidence.map(String)
          : [],
      }
    case "done":
      return {
        kind: "done",
        effectId: "",
        result: {
          termination: String(asObject(raw.result).termination ?? "completed"),
          turnsUsed: Number(asObject(raw.result).turns_used ?? 0),
          totalTokensUsed: Number(asObject(raw.result).total_tokens_used ?? 0),
        },
      }
    default:
      return null
  }
}

/** Wrap a scripted kernel so host projections can call `applyHostEvent`. */
export function wrapScriptedKernel(fake: ScriptedKernel, operationId = "wasm-scripted-operation") {
  const journal = new InMemoryKernelJournal()
  let terminal = false
  let turns = 0
  const observations: Array<Record<string, unknown>> = []
  const messages: unknown[] = []

  const runtime = {
    operationId,
    journal,
    turn: () => turns,
    isTerminal: () => terminal,
    recoveryContentBytes: () => 32_768,
    preservedRefs: () => [] as string[],
    drainNewMessages: () => messages.splice(0),
    drainHostObservations: () => observations.splice(0) as never,
    resumeAction: () => null,
    async restore() {},
    async startAgent(task: Record<string, unknown>, runSpec?: Record<string, unknown>) {
      return runtime.applyHostEvent({ kind: "start_run", task, ...(runSpec ? { run_spec: runSpec } : {}) })
    },
    async startWorkflow(spec: Record<string, unknown>) {
      return runtime.applyHostEvent({ kind: "load_workflow", spec })
    },
    async applyHostEvent(event: Record<string, unknown>) {
      const step = JSON.parse(fake.step(JSON.stringify({
        operation_id: operationId,
        event,
      }))) as {
        actions?: Array<Record<string, unknown>>
        observations?: Array<Record<string, unknown>>
      }
      for (const obs of step.observations ?? []) {
        observations.push(obs)
        if (obs.kind === "workflow_completed" || String(asObject(obs).kind) === "done") {
          terminal = true
        }
      }
      const actions = step.actions ?? []
      if (actions.length === 0) {
        if (terminal) {
          return {
            kind: "done" as const,
            effectId: "",
            result: { termination: "completed", turnsUsed: turns, totalTokensUsed: 0 },
          }
        }
        return null
      }
      const action = mapAction(actions[0]!)
      if (action?.kind === "done") terminal = true
      if (action?.kind === "call_provider") turns += 1
      return action
    },
  }
  return runtime
}
