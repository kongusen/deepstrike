/**
 * High-level facades for the two bread-and-butter cases, so a caller doesn't have to assemble
 * `RuntimeRunner` + session log + execution plane + `collectText` by hand (integration feedback #4:
 * the package exports ~150 symbols and the canonical entry point for common work wasn't discoverable).
 *
 * - `runAgent`   — one prompt, one model, the text back. The 90%-case single-agent call.
 * - `runFanout`  — run N tasks in parallel, then synthesize, from a stateless request handler. Drives
 *                  the kernel-gated DAG via the standalone `runWorkflow` path (governed · resumable),
 *                  instead of hand-rolling a multi-runner fan-out.
 *
 * Both build a throwaway `RuntimeRunner` with sensible defaults; pass `sessionLog` / `executionPlane`
 * to opt into persistence or custom tools. Reach for the underlying `RuntimeRunner` directly when you
 * need streaming events, signals, memory, or governance hooks.
 */
import { RuntimeRunner, collectText } from "./runner.js"
import { LocalExecutionPlane } from "./execution-plane.js"
import { InMemorySessionLog } from "./session-log.js"
import type { ExecutionPlane } from "./execution-plane.js"
import type { SessionLog } from "./session-log.js"
import type { LLMProvider } from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { WorkflowSpec, WorkflowTaskSpec, KernelAgentRole } from "../types/agent.js"
import { fanoutSynthesize } from "../types/agent.js"

/** Shared knobs for the facade entry points. */
export interface RunAgentOptions {
  provider: LLMProvider
  goal: string
  systemPrompt?: string
  tools?: RegisteredTool[]
  sessionId?: string
  maxTokens?: number
  maxTurns?: number
  /** Persist the run (resume / audit). Defaults to an in-memory, throwaway log. */
  sessionLog?: SessionLog
  /** Custom execution plane (tools, sandboxing). Overrides `tools` when both are given. */
  executionPlane?: ExecutionPlane
}

/**
 * Run a single agent to completion and return its final text — `RuntimeRunner` + `run` + `collectText`
 * in one call. Register tools by passing `tools`; everything else has a working default.
 */
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
  /** One parallel worker per task. A string is shorthand for `{ goal }`. */
  tasks: WorkflowTaskSpec[]
  /** Final synthesis prompt; runs once after every worker completes, with their outputs in context. */
  synthesize: string
  /** Role for the parallel workers (default `explore`) and the synthesis node (default `plan`). */
  workerRole?: KernelAgentRole
  synthesisRole?: KernelAgentRole
  sessionId?: string
  maxTokens?: number
  maxTurns?: number
  sessionLog?: SessionLog
  executionPlane?: ExecutionPlane
}

/**
 * Parallel fan-out → synthesize, driven by the kernel-gated DAG (the standalone `runWorkflow` path):
 * each task becomes a fresh-context worker node, and a final synthesis node depends on all of them.
 * Returns the synthesis text plus every node's raw output. Safe to call from a stateless handler — it
 * bootstraps and tears down its own kernel.
 */
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
  // The synthesis node is the last spec node; the kernel ids nodes `wf-node{index}`. Prefer that id,
  // but fall back to the last completed node's output so a kernel id-scheme change can't silently
  // return an empty synthesis.
  const synthesisId = `wf-node${opts.tasks.length}`
  const completed = outcome.nodeOutcomes.filter(node =>
    node.status === "completed" || node.status === "completed_partial")
  const lastCompleted = completed[completed.length - 1]?.nodeId
  const synthesis = outcome.outputs[synthesisId] ?? (lastCompleted ? outcome.outputs[lastCompleted] : undefined) ?? ""
  return { synthesis, outputs: outcome.outputs }
}
