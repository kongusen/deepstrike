// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export class LoopStateMachine {
  private terminal = false
  private turn = 0
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
  takeObservations(): unknown[] { return [] }
  isTerminal(): boolean { return this.terminal }

  start(_task: unknown) {
    this.turn = 0
    this.terminal = false
    return { kind: "call_llm", messages: [{ role: "user", content: "test" }], tools: [] }
  }

  feedLlmResponse(_msg: unknown) {
    this.terminal = true
    return { kind: "done", result: { turnsUsed: 1, totalTokensUsed: 100, termination: "completed" } }
  }

  feedToolResults(_results: unknown[]) {
    return { kind: "call_llm", messages: [], tools: [] }
  }

  feedSkillsLoaded(_skills: unknown[]) {
    return { kind: "call_llm", messages: [], tools: [] }
  }

  feedTimeout() {
    this.terminal = true
    return { kind: "done", result: { turnsUsed: this.turn, totalTokensUsed: 0, termination: "timeout" } }
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
