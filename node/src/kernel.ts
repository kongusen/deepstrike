import { createRequire } from "module"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { Message, RenderedContext } from "./types.js"

export interface GovernanceVerdict {
  kind: "allow" | "deny" | "rate_limited" | "ask_user"
  reason?: string
  retryAfterMs?: number
}

/**
 * M2 资源配额 — declarative resource limits enforced at the kernel's single syscall trap.
 *
 * Installed through the versioned JSON event ABI (`set_resource_quota`), not a side-channel
 * setter, so quota config is replayable and session-loggable like governance/scheduler config.
 * Every field is optional; an omitted field imposes no limit, and omitting the quota entirely
 * preserves the pre-M2 behavior of admitting all spawn / memory-write syscalls.
 */
export interface MemoryWriteRateLimit {
  maxWrites: number
  windowMs: number
}

export interface ResourceQuota {
  /** Max sub-agents in the `running` state at once; further spawns are denied while at cap.
   *  Instantaneous — vehicle-scoped (cannot span stateless replicas). */
  maxConcurrentSubagents?: number
  /** L1 (RunGroup): max sub-agents spawned *cumulatively* across the governance domain. With a
   *  `runGroup`, this spans N stateless top-level runs (seeded/charged via the group ledger). */
  maxTotalSubagents?: number
  /** Max sub-agent nesting depth (direct children of the root loop are depth 1). */
  maxSpawnDepth?: number
  /** Rolling-window memory-write rate limit: at most `maxWrites` per any `windowMs` span. */
  memoryWritesPerWindow?: MemoryWriteRateLimit
}

/**
 * Long-term memory policy — declarative knobs for the kernel's memory subsystem.
 *
 * Installed through the versioned JSON event ABI (`set_memory_policy`), the same channel as
 * governance / scheduler / quota config, so memory configuration is replayable and
 * session-loggable rather than a side-channel setter. Installing the policy is opt-in and
 * kernel-enforced; omitted fields fall back to the kernel defaults (empty path, 2-day stale
 * warning, top-5 retrieval, validation on). Enabling memory is still `dreamStore` + `agentId`.
 */
export interface MemoryPolicy {
  /** Filesystem root the SDK uses to persist/scan memories; carried for SDK recall I/O. */
  memoryPath?: string
  /** Age after which a recalled memory is flagged stale (days); consumed SDK-side. */
  staleWarningDays?: number
  /** Upper bound on retrieval breadth: the kernel clamps `query_memory` top-k to this. */
  retrievalTopK?: number
  /** When false, the kernel admits every `write_memory` without validation. */
  validationEnabled?: boolean
  /** Override the kernel's `write_memory` content-size limit (bytes). */
  maxContentBytes?: number
  /** Override the kernel's `write_memory` name-length limit. */
  maxNameLength?: number
}

export interface GovernanceInstance {
  setIdentity(agentId: string, sessionId: string): void
  addPermissionRule(pattern: string, action: "allow" | "deny" | "ask_user"): void
  blockTool(name: string): void
  setRateLimit(toolName: string, maxCalls: number, windowMs: bigint): void
  requireParam(toolName: string, paramPath: string): void
  allowParamValues(toolName: string, paramPath: string, allowedValues: string[]): void
  limitParamRange(toolName: string, paramPath: string, min?: number, max?: number): void
  setTime(nowMs: bigint): void
  evaluate(toolName: string, argsJson: string): GovernanceVerdict
}

export interface RuntimeSignal {
  id: string
  source: "cron" | "gateway" | "heartbeat" | "custom"
  signalType: "event" | "job" | "alert"
  urgency: "low" | "normal" | "high" | "critical"
  summary: string
  payload: string
  dedupeKey?: string
  timestampMs: number
}

interface SignalRouterInstance {
  ingest(signal: RuntimeSignal, isRunning: boolean): string
  next(): RuntimeSignal | null
}

interface NativeCriterion {
  text: string
  required: boolean
  weight?: number
}

export interface Verdict {
  passed: boolean
  overallScore: number
  feedback: string
  details: Array<{
    criterion: string
    passed: boolean
    score: number
    feedback: string
  }>
  skillCandidate?: {
    name: string
    description: string
    whenToUse?: string
    content: string
  }
}

interface IdlePipelineAction {
  kind: "synthesize_insights" | "commit_memories" | "noop" | "aborted"
  messages?: Message[]
  curationResult?: {
    toAdd?: Array<{ text: string; score: number; metadata: string }>
    toRemoveIndices?: number[]
    stats?: {
      insightsProcessed?: number
      duplicatesRemoved?: number
      conflictsResolved?: number
      entriesAdded?: number
    }
  }
  runResult?: {
    sessionsProcessed: number
    insightsExtracted: number
  }
}

interface IdlePipelineInstance {
  feedTrigger(
    sessions: Array<{
      sessionId: string
      agentId: string
      messages: Message[]
      metadata: string
      createdAtMs: number
      updatedAtMs: number
    }>,
    existingMemories: Array<{ text: string; score: number; metadata: string }>,
    nowMs: number,
  ): IdlePipelineAction
  feedSynthesisResult(content: string): IdlePipelineAction
}

export interface KernelRuntimeInstance {
  step(inputJson: string): string
  snapshot(): string
  restore(snapshotJson: string): void
  isTerminal(): boolean
  turn(): number
  /** L1 (RunGroup): cumulative sub-agent spawns this run, for charging the group ledger at run end. */
  localSubagentsSpawned(): number
  recoveryContentBytes(): number
  render(): RenderedContext
  drainNewMessages(): Message[]
  preservedRefs(): string[]
}

interface KernelModule {
  Governance: new (defaultAction?: "allow" | "deny" | "ask_user") => GovernanceInstance
  KernelRuntime: new (policy: {
    maxTokens: number
    maxTurns?: number
    maxTotalTokens?: bigint
    timeoutMs?: bigint
  }) => KernelRuntimeInstance
  SignalRouter: new (maxQueueSize: number) => SignalRouterInstance
  // Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class).
  buildEvalMessages(goal: string, criteria: NativeCriterion[], result: string, attempt: number, extractSkillOnPass: boolean): Message[]
  parseVerdict(content: string): Verdict
  verdictOutputSchema(extractSkillOnPass: boolean): string
  IdlePipeline: new (agentId: string) => IdlePipelineInstance
}

const cjsRequire = createRequire(import.meta.url)
let cachedKernel: KernelModule | undefined

function resolveCoreModule(): string {
  const localCore = join(dirname(fileURLToPath(import.meta.url)), "../../crates/deepstrike-node")
  if (existsSync(join(localCore, "index.js"))) return localCore
  return "@deepstrike/core"
}

export function getKernel(): KernelModule {
  if (!cachedKernel) {
    cachedKernel = cjsRequire(resolveCoreModule()) as KernelModule
  }
  return cachedKernel
}
