import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { RegisteredTool } from "../../src/tools/index.js"
import type { AsyncSummarizer, LLMProvider } from "../../src/types.js"
import type { ToolSuspendEvent } from "../../src/types.js"

export function createRunner(
  provider: LLMProvider,
  tools: RegisteredTool[] = [],
  opts: {
    maxTokens?: number
    maxTurns?: number
    sessionLog?: InMemorySessionLog
    onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
    governance?: {
      setTime?(nowMs: bigint): void
      evaluate(name: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
    }
    asyncSummarizer?: AsyncSummarizer
  } = {},
): { runner: RuntimeRunner; sessionLog: InMemorySessionLog; plane: LocalExecutionPlane } {
  const sessionLog = opts.sessionLog ?? new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of tools) plane.register(t)
  const runner = new RuntimeRunner({
    provider,
    sessionLog,
    executionPlane: plane,
    maxTokens: opts.maxTokens ?? 2048,
    maxTurns: opts.maxTurns ?? 25,
    onToolSuspend: opts.onToolSuspend,
    governance: opts.governance,
    asyncSummarizer: opts.asyncSummarizer,
  })
  return { runner, sessionLog, plane }
}
