/** Ambient types when `@deepstrike/wasm-kernel` is not installed (e.g. `tsc` without `build:wasm`). */
declare module "@deepstrike/wasm-kernel" {
  export class KernelRuntime {
    constructor(policy: { maxTokens: number; maxTurns?: number; maxTotalTokens?: number; timeoutMs?: number })
    step(inputJson: string): string
    snapshot(): string
    restore(snapshotJson: string): void
    isTerminal(): boolean
    turn(): number
    recoveryContentBytes(): number
    render(): import("./types.js").RenderedContext
    drainNewMessages(): import("./types.js").Message[]
    preservedRefs(): string[]
  }

  export class SignalRouter {
    constructor(maxQueueSize: number)
    ingest(signal: unknown, isRunning: boolean): string
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

  export class IdlePipeline {
    constructor(agentId: string)
    feedTrigger(sessions: unknown[], memories: unknown[], nowMs: number): { kind: string; messages?: unknown[]; curationResult?: unknown; runResult?: unknown }
    feedSynthesisResult(content: string): { kind: string; curationResult?: unknown; runResult?: unknown }
  }

  // Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class).
  export function buildEvalMessages(goal: string, criteria: unknown[], result: string, attempt: number, extractSkillOnPass: boolean): import("./types.js").Message[]
  export function parseVerdict(content: string): { passed: boolean; overallScore: number; feedback: string; details: unknown[]; skillCandidate?: unknown }
  export function verdictOutputSchema(extractSkillOnPass: boolean): string
}
