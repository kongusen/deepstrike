// High-level facades (parity with the Node SDK's runAgent/runFanout) so the common cases don't require
// assembling RuntimeRunner + session log + execution plane + collectText by hand. Browser/edge-friendly.
import { RuntimeRunner, collectText } from "./runner.js"
import { LocalExecutionPlane } from "./execution-plane.js"
import { InMemorySessionLog } from "./session-log.js"
import type { ExecutionPlane } from "./execution-plane.js"
import type { SessionLog } from "./session-log.js"
import type { LLMProvider } from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { WorkflowSpec, WorkflowTaskSpec, KernelAgentRole } from "./types/agent.js"
import { fanoutSynthesize } from "./types/agent.js"

export interface RunAgentOptions {
  provider: LLMProvider
  goal: string
  systemPrompt?: string
  tools?: RegisteredTool[]
  sessionId?: string
  maxTokens?: number
  maxTurns?: number
  sessionLog?: SessionLog
  executionPlane?: ExecutionPlane
}

/** Run a single agent to completion and return its final text. */
export async function runAgent(opts: RunAgentOptions): Promise<string> {
  const plane =
    opts.executionPlane ??
    (opts.tools ?? []).reduce((p, t) => p.register(t), new LocalExecutionPlane())
  const runner = new RuntimeRunner({
    provider: opts.provider,
    executionPlane: plane,
    sessionLog: opts.sessionLog ?? new InMemorySessionLog(),
    maxTokens: opts.maxTokens ?? 32_000,
    ...(opts.maxTurns !== undefined ? { maxTurns: opts.maxTurns } : {}),
    ...(opts.systemPrompt !== undefined ? { systemPrompt: opts.systemPrompt } : {}),
  })
  return collectText(runner.run({ sessionId: opts.sessionId ?? `agent-${crypto.randomUUID()}`, goal: opts.goal }))
}

export interface RunFanoutOptions {
  provider: LLMProvider
  tasks: WorkflowTaskSpec[]
  synthesize: string
  workerRole?: KernelAgentRole
  synthesisRole?: KernelAgentRole
  sessionId?: string
  maxTokens?: number
  maxTurns?: number
  sessionLog?: SessionLog
  executionPlane?: ExecutionPlane
}

/** Parallel fan-out → synthesize over the kernel-gated DAG (standalone runWorkflow). */
export async function runFanout(opts: RunFanoutOptions): Promise<{ synthesis: string; outputs: Record<string, string> }> {
  const runner = new RuntimeRunner({
    provider: opts.provider,
    executionPlane: opts.executionPlane ?? new LocalExecutionPlane(),
    sessionLog: opts.sessionLog ?? new InMemorySessionLog(),
    maxTokens: opts.maxTokens ?? 32_000,
    ...(opts.maxTurns !== undefined ? { maxTurns: opts.maxTurns } : {}),
  })
  // W-N8: build the spec via the ONE fanout template (it pins the pattern's isolation /
  // context-inheritance choices — read-only system-only workers, full-context synthesizer — which
  // this facade used to silently drop), then apply the caller's role overrides on top.
  const spec: WorkflowSpec = fanoutSynthesize(opts.tasks, opts.synthesize)
  if (opts.workerRole) {
    for (const node of spec.nodes.slice(0, opts.tasks.length)) node.role = opts.workerRole
  }
  if (opts.synthesisRole) spec.nodes[spec.nodes.length - 1].role = opts.synthesisRole
  const outcome = await runner.runWorkflow(spec, opts.sessionId ? { sessionId: opts.sessionId } : undefined)
  const synthesisId = `wf-node${opts.tasks.length}`
  const lastCompleted = outcome.completed[outcome.completed.length - 1]
  const synthesis = outcome.outputs[synthesisId] ?? (lastCompleted ? outcome.outputs[lastCompleted] : undefined) ?? ""
  return { synthesis, outputs: outcome.outputs }
}
