import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { RegisteredTool } from "../../src/tools/index.js"
import type { AsyncSummarizer, LLMProvider, PermissionRequestEvent, PermissionResponse } from "../../src/types.js"
import type { ToolSuspendEvent } from "../../src/types.js"
import type { GovernancePolicy } from "../../src/governance.js"
import type { DreamStore } from "../../src/memory/protocols.js"
import type { ArchiveStore } from "../../src/runtime/archive.js"
import type { OsProfile } from "../../src/runtime/os-profile.js"

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
    onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
    onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
    governance?: {
      setTime?(nowMs: bigint): void
      evaluate(name: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
    }
    governancePolicy?: GovernancePolicy
    attentionPolicy?: { maxQueueSize?: number }
    osProfile?: OsProfile
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
    agentId: opts.agentId,
    dreamStore: opts.dreamStore,
    compressionStore: opts.compressionStore,
    onToolSuspend: opts.onToolSuspend,
    onPermissionRequest: opts.onPermissionRequest,
    governance: opts.governance,
    governancePolicy: opts.governancePolicy,
    attentionPolicy: opts.attentionPolicy,
    osProfile: opts.osProfile,
    asyncSummarizer: opts.asyncSummarizer,
  })
  return { runner, sessionLog, plane }
}
