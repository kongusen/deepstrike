import { createRequire } from "module"
import { existsSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import type { Message } from "./types.js"

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
  /** Max nodes in one in-kernel workflow DAG, including dynamically submitted nodes. */
  maxWorkflowNodes?: number
  /** Rolling-window memory-write rate limit: at most `maxWrites` per any `windowMs` span. */
  memoryWritesPerWindow?: MemoryWriteRateLimit
}

/**
 * Long-term memory policy — declarative knobs for the kernel's memory subsystem.
 *
 * Included in canonical operation configuration, so memory policy is replayable and
 * kernel-enforced. Omitted fields retain the canonical defaults. Host storage belongs to the
 * configured `DreamStore`, never to this contract.
 */
export interface MemoryPolicy {
  /** Age after which a recalled memory is flagged stale (days). */
  staleWarningDays?: number
  /** Upper bound on retrieval breadth: the kernel clamps `query_memory` top-k to this. */
  retrievalTopK?: number
  /** When false, the kernel admits every `write_memory` without validation. */
  validationEnabled?: boolean
  /** Override the kernel's `write_memory` content-size limit (bytes). */
  maxContentBytes?: number
  /** Override the kernel's `write_memory` name-length limit. */
  maxNameLength?: number
  /** M4: recall count at which the kernel emits an advisory (edge-triggered)
   *  `promotion_suggested` for a recalled record. Omitted = suggestions disabled. */
  promotionRecallThreshold?: number
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
  recipient?: string
  deadlineMs?: number
  coalesceKey?: string
  coalescedCount?: number
  timestampMs: number
}

export type SignalRouterLifecycle = "ready" | "running" | "suspended" | "done"

interface SignalRouterInstance {
  ingest(signal: RuntimeSignal, lifecycle: SignalRouterLifecycle): string
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

export interface CanonicalPrepared {
  status: "prepared"
  prepareToken: string
  stepSeq: string
  expectedHead?: string
  recordDigest: string
  recordBytes: Buffer
  plannedStepJson: string
}

export interface CanonicalReplayed {
  status: "replayed"
  stepSeq: string
  expectedHead?: string
  recordDigest: string
  recordBytes?: Buffer
  plannedStepJson?: string
}

export interface CanonicalRejected {
  status: "rejected"
  faultJson: string
}

/** Closed §7.13 result; only `prepared` carries a token that may be committed or aborted. */
export type CanonicalPreparation = CanonicalPrepared | CanonicalReplayed | CanonicalRejected

export interface CanonicalCommit {
  stepSeq: string
  recordDigest: string
  plannedStepJson: string
  checkpointAdviceJson?: string
}

export interface CanonicalCheckpoint {
  checkpointBytes: Buffer
  throughStepSeq: string
  coveredHead: string
  stateDigest: string
  ackToken: string
}

export interface CanonicalRestoreCost {
  recordsBeforeCheckpoint: string
  tailInputsReplayed: string
  recordsAfterCheckpoint: string
  bytesRead: string
}

export interface CanonicalKernelInstance {
  prepare(inputJson: string): CanonicalPreparation
  commit(prepareToken: string, appendedHead: string): CanonicalCommit
  abort(prepareToken: string): void
  checkpointCandidate(): CanonicalCheckpoint
  checkpointRebase(checkpointBytes: Buffer): CanonicalCheckpoint
  ackCheckpoint(throughStepSeq: string, coveredHead: string): void
  /** Replaces native state in place; the JavaScript handle retains its identity. */
  restore(checkpointBytes: Buffer | undefined, recordBytes: Buffer[]): CanonicalRestoreCost
  lifecycle(): "created" | "configured" | "running" | "suspended" | "completed" | "cancelled" | "failed"
  pendingEffectsJson(): string
  terminalJson(): string | undefined
}

interface KernelModule {
  Governance: new (defaultAction?: "allow" | "deny" | "ask_user") => GovernanceInstance
  CanonicalKernel: new () => CanonicalKernelInstance
  kernelAbiVersion(): number
  SignalRouter: new (maxQueueSize: number) => SignalRouterInstance
  // Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class).
  buildEvalMessages(goal: string, criteria: NativeCriterion[], result: string, attempt: number, extractSkillOnPass: boolean): Message[]
  parseVerdict(content: string): Verdict
  verdictOutputSchema(extractSkillOnPass: boolean): string
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
