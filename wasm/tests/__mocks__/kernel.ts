// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export class LoopStateMachine {
  private terminal = false
  private turn = 0
  private maxTurns: number

  constructor(policy: { maxTokens: number; maxTurns?: number }) {
    this.maxTurns = policy.maxTurns ?? 25
  }

  setTools(_tools: unknown[]): void {}
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
