// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export class DeepStrikeRuntime {
  private terminal = false
  turn = 0
  private phase = 0
  private maxTurns: number

  constructor(policy: { maxTokens: number; maxTurns?: number }) {
    this.maxTurns = policy.maxTurns ?? 25
  }

  setTools(_tools: unknown[]): void {}
  setAvailableSkills(_skills: unknown[]): void {}
  setMemoryEnabled(_enabled: boolean): void {}
  setKnowledgeEnabled(_enabled: boolean): void {}
  addSystemMessage(_content: string, _tokens: number): void {}
  addMemoryMessage(_content: string, _tokens: number): void {}
  addHistoryMessage(_message: unknown, _tokens: number): void {}
  preloadHistory(_messages: unknown[]): void {}
  resumeAfterPreload() {
    return { kind: "call_llm", context: { systemText: "", turns: [{ role: "user", content: "resume" }] }, tools: [] }
  }
  drainNewMessages(): unknown[] { return [] }
  takeObservations(): unknown[] { return [] }
  isTerminal(): boolean { return this.terminal }

  start(_task: unknown) {
    this.turn = 0
    this.phase = 0
    this.terminal = false
    return { kind: "call_llm", context: { systemText: "", turns: [{ role: "user", content: "test" }] }, tools: [] }
  }

  feedLlmResponse(msg: unknown) {
    const toolCalls = (msg as { toolCalls?: unknown[] })?.toolCalls ?? []
    if (this.phase === 0 && toolCalls.length > 0) {
      this.phase = 1
      return { kind: "execute_tools", calls: toolCalls }
    }
    this.terminal = true
    return { kind: "done", result: { turnsUsed: 2, totalTokensUsed: 100, termination: "completed" } }
  }

  feedToolResults(_results: unknown[]) {
    return { kind: "call_llm", context: { systemText: "", turns: [] }, tools: [] }
  }

  feedSkillsLoaded(_skills: unknown[]) {
    return { kind: "call_llm", context: { systemText: "", turns: [] }, tools: [] }
  }

  feedTimeout() {
    this.terminal = true
    return { kind: "done", result: { turnsUsed: this.turn, totalTokensUsed: 0, termination: "timeout" } }
  }

  get result() {
    return this.terminal ? { termination: "completed", turnsUsed: 2, totalTokensUsed: 100 } : undefined
  }
}

export class IdlePipeline {
  constructor(_agentId: string) {}
  feedTrigger() {
    return { kind: "noop" }
  }
  feedSynthesisResult(_content: string) {
    return { kind: "noop" }
  }
}

export class Governance {
  blockTool(_name: string): void {}
  setTime(_nowMs: number): void {}
  evaluate(_toolName: string, _argsJson: string) {
    return { kind: "allow" as const }
  }
}

export class SignalRouter {
  constructor(_maxQueueSize: number) {}
  ingest(_signal: unknown, _isRunning: boolean): string { return "ignore" }
  next(): null { return null }
  depth(): number { return 0 }
  clearDedup(): void {}
}

export class EvalPipeline {
  constructor(_options: { extractSkillOnPass: boolean }) {}

  feedOutcome(_goal: string, _criteria: unknown[], _result: string, _attempt: number) {
    return { kind: "evaluate", messages: [] }
  }

  feedEvalResult(_content: string) {
    return {
      kind: "done",
      passed: true,
      overallScore: 1,
      feedback: "",
      details: [],
    }
  }

  reset(): void {}
  isIdle(): boolean { return true }
}
