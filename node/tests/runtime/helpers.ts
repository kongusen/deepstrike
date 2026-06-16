import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { RegisteredTool } from "../../src/tools/index.js"
import type { AsyncSummarizer, DreamSummarizer, LLMProvider, PermissionRequestEvent, PermissionResponse } from "../../src/types.js"
import type { ToolSuspendEvent } from "../../src/types.js"
import type { GovernancePolicy } from "../../src/governance.js"
import type { DreamStore } from "../../src/memory/protocols.js"
import type { ArchiveStore } from "../../src/runtime/archive.js"
import type { LargeResultSpool } from "../../src/runtime/large-result-spool.js"

export { tool } from "../../src/tools/index.js"

export function createRunner(
  provider: LLMProvider,
  tools: RegisteredTool[] = [],
  opts: {
    maxTokens?: number
    maxTurns?: number
    sessionLog?: InMemorySessionLog
    agentId?: string
    dreamStore?: DreamStore
    compressionStore?: ArchiveStore
    resultSpool?: LargeResultSpool
    onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
    onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
    governancePolicy?: GovernancePolicy
    attentionPolicy?: { maxQueueSize?: number }
    asyncSummarizer?: AsyncSummarizer
    dreamSummarizer?: DreamSummarizer
    dreamProvider?: LLMProvider
    allowedToolIds?: string[]
    onTurnMetrics?: (m: import("../../src/runtime/runner.js").TurnMetrics) => void
    skillDir?: string
    stableCoreToolIds?: string[]
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
    agentId: opts.agentId,
    dreamStore: opts.dreamStore,
    compressionStore: opts.compressionStore,
    resultSpool: opts.resultSpool,
    onToolSuspend: opts.onToolSuspend,
    onPermissionRequest: opts.onPermissionRequest,
    governancePolicy: opts.governancePolicy,
    attentionPolicy: opts.attentionPolicy,
    asyncSummarizer: opts.asyncSummarizer,
    dreamSummarizer: opts.dreamSummarizer,
    dreamProvider: opts.dreamProvider,
    allowedToolIds: opts.allowedToolIds,
    onTurnMetrics: opts.onTurnMetrics,
    skillDir: opts.skillDir,
    stableCoreToolIds: opts.stableCoreToolIds,
  })
  return { runner, sessionLog, plane }
}
