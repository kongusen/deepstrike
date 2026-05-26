/** Ambient types when `@deepstrike/wasm-kernel` is not installed (e.g. `tsc` without `build:wasm`). */
declare module "@deepstrike/wasm-kernel" {
  export class DeepStrikeRuntime {
    constructor(policy: { maxTokens: number; maxTurns?: number; timeoutMs?: bigint })
    turn: number
    setTools(tools: unknown[]): void
    setAvailableSkills(skills: unknown[]): void
    setMemoryEnabled(enabled: boolean): void
    setKnowledgeEnabled(enabled: boolean): void
    addSystemMessage(content: string, tokens: number): void
    addMemoryMessage(content: string, tokens: number): void
    addHistoryMessage(message: unknown, tokens: number): void
    preloadHistory(messages: unknown[]): void
    resumeAfterPreload(): { kind: string; context?: unknown; tools?: unknown[]; calls?: unknown[]; result?: unknown }
    drainNewMessages(): unknown[]
    takeObservations(): Array<{ kind: string }>
    isTerminal(): boolean
    start(task: { goal: string; criteria: string[] }): { kind: string; context?: unknown; tools?: unknown[]; calls?: unknown[]; result?: unknown }
    feedLlmResponse(message: unknown): { kind: string; context?: unknown; tools?: unknown[]; calls?: unknown[]; result?: unknown }
    feedToolResults(results: unknown[]): { kind: string; context?: unknown; tools?: unknown[]; calls?: unknown[]; result?: unknown }
    feedTimeout(): { kind: string; context?: unknown; tools?: unknown[]; calls?: unknown[]; result?: unknown }
    readonly result?: { termination: string; turnsUsed: number; totalTokensUsed: bigint }
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
