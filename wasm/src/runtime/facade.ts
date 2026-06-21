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
  const workerRole = opts.workerRole ?? "explore"
  const spec: WorkflowSpec = {
    nodes: [
      ...opts.tasks.map(task => ({ task, role: workerRole })),
      { task: opts.synthesize, role: opts.synthesisRole ?? "plan", dependsOn: opts.tasks.map((_, i) => i) },
    ],
  }
  const outcome = await runner.runWorkflow(spec, opts.sessionId ? { sessionId: opts.sessionId } : undefined)
  const synthesisId = `wf-node${opts.tasks.length}`
  const lastCompleted = outcome.completed[outcome.completed.length - 1]
  const synthesis = outcome.outputs[synthesisId] ?? (lastCompleted ? outcome.outputs[lastCompleted] : undefined) ?? ""
  return { synthesis, outputs: outcome.outputs }
}
