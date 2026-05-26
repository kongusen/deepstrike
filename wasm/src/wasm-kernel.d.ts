/** Ambient types when `@deepstrike/wasm-kernel` is not installed (e.g. `tsc` without `build:wasm`). */
declare module "@deepstrike/wasm-kernel" {
  export class KernelRuntime {
    constructor(policy: { maxTokens: number; maxTurns?: number; timeoutMs?: bigint })
    step(inputJson: string): string
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
    blockTool(name: string): void
    setTime(nowMs: number): void
    evaluate(toolName: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
  }

  export class IdlePipeline {
    constructor(agentId: string)
    feedTrigger(sessions: unknown[], memories: unknown[], nowMs: number): { kind: string; messages?: unknown[]; curationResult?: unknown; runResult?: unknown }
    feedSynthesisResult(content: string): { kind: string; curationResult?: unknown; runResult?: unknown }
  }

  export class EvalPipeline {
    constructor(options: { extractSkillOnPass: boolean })
    feedOutcome(goal: string, criteria: unknown[], result: string, attempt: number): { kind: string; messages?: unknown[] }
    feedEvalResult(content: string): { kind: string; passed?: boolean; overallScore?: number; feedback?: string; details?: unknown[]; skillCandidate?: unknown }
    reset(): void
    isIdle(): boolean
  }
}
