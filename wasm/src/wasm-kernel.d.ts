/** Ambient types when `@deepstrike/wasm-kernel` is not installed (e.g. `tsc` without `build:wasm`). */
declare module "@deepstrike/wasm-kernel" {
  export type SignalRouterLifecycle = "ready" | "running" | "suspended" | "done"

  export interface CanonicalPrepared {
    status: "prepared"
    prepareToken: string
    stepSeq: string
    expectedHead?: string
    recordDigest: string
    recordBytes: Uint8Array
    plannedStepJson: string
  }

  export interface CanonicalReplayed {
    status: "replayed"
    stepSeq: string
    expectedHead?: string
    recordDigest: string
    recordBytes?: Uint8Array
    plannedStepJson?: string
  }

  export interface CanonicalRejected {
    status: "rejected"
    faultJson: string
  }

  export type CanonicalPreparation = CanonicalPrepared | CanonicalReplayed | CanonicalRejected

  export interface CanonicalCommit {
    stepSeq: string
    recordDigest: string
    plannedStepJson: string
    checkpointAdviceJson?: string
  }

  export interface CanonicalCheckpoint {
    checkpointBytes: Uint8Array
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

  /** Durable canonical transition protocol. Deliberately has no direct `step` method. */
  export class CanonicalKernel {
    constructor()
    prepare(inputJson: string): CanonicalPreparation
    commit(prepareToken: string, appendedHead: string): CanonicalCommit
    abort(prepareToken: string): void
    checkpointCandidate(): CanonicalCheckpoint
    checkpointRebase(checkpointBytes: Uint8Array): CanonicalCheckpoint
    ackCheckpoint(throughStepSeq: string, coveredHead: string): void
    /** Replaces native state in place; the JavaScript handle retains its identity. */
    restore(checkpointBytes: Uint8Array | undefined, recordBytes: Uint8Array[]): CanonicalRestoreCost
    lifecycle(): "created" | "configured" | "running" | "suspended" | "completed" | "cancelled" | "failed"
    pendingEffectsJson(): string
    terminalJson(): string | undefined
  }

  export function kernelAbiVersion(): number

  export class SignalRouter {
    constructor(maxQueueSize: number)
    ingest(signal: unknown, lifecycle: SignalRouterLifecycle): string
    next(): { urgency: string } | null
  }

  export class Governance {
    constructor(defaultAction?: string)
    setIdentity(agentId: string, sessionId: string): void
    addPermissionRule(pattern: string, action: string): void
    blockTool(name: string): void
    setRateLimit(toolName: string, maxCalls: number, windowMs: number): void
    requireParam(toolName: string, paramPath: string): void
    allowParamValues(toolName: string, paramPath: string, allowedValues: string[]): void
    limitParamRange(toolName: string, paramPath: string, min?: number, max?: number): void
    setTime(nowMs: number): void
    evaluate(toolName: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
  }

  // Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class).
  export function buildEvalMessages(goal: string, criteria: unknown[], result: string, attempt: number, extractSkillOnPass: boolean): import("./types.js").Message[]
  export function parseVerdict(content: string): { passed: boolean; overallScore: number; feedback: string; details: unknown[]; skillCandidate?: unknown }
  export function verdictOutputSchema(extractSkillOnPass: boolean): string
}
