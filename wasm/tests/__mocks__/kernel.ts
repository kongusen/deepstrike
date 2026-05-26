// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export class KernelRuntime {
  private terminal = false
  private phase = 0
  private maxTurns: number
  private rendered = { systemText: "", turns: [] as unknown[] }
  private messages: unknown[] = []

  constructor(policy: { maxTokens: number; maxTurns?: number }) {
    this.maxTurns = policy.maxTurns ?? 25
  }

  step(inputJson: string): string {
    const input = JSON.parse(inputJson) as { event?: Record<string, unknown> }
    const event = input.event ?? {}
    const actions: Array<Record<string, unknown>> = []

    switch (event.kind) {
      case "start_run":
        this.phase = 0
        this.terminal = false
        this.rendered = { systemText: "", turns: [{ role: "user", content: "test" }] }
        actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
        break
      case "resume":
        this.rendered = { systemText: "", turns: [{ role: "user", content: "resume" }] }
        actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
        break
      case "provider_result": {
        const message = (event.message as Record<string, unknown>) ?? {}
        this.messages.push(message)
        const toolCalls = (message.tool_calls as unknown[]) ?? []
        if (this.phase === 0 && toolCalls.length > 0) {
          this.phase = 1
          actions.push({ kind: "execute_tool", calls: toolCalls })
        } else {
          this.terminal = true
          actions.push({
            kind: "done",
            result: { turns_used: 2, total_tokens_used: 100, termination: "completed" },
          })
        }
        break
      }
      case "tool_results":
        actions.push({ kind: "call_provider", context: { systemText: "", turns: [] }, tools: [] })
        break
      case "timeout":
        this.terminal = true
        actions.push({
          kind: "done",
          result: { turns_used: this.turn(), total_tokens_used: 0, termination: "timeout" },
        })
        break
      case "force_compact":
        break
      default:
        break
    }

    return JSON.stringify({ version: 1, actions, observations: [] })
  }

  isTerminal(): boolean { return this.terminal }
  turn(): number { return this.phase }
  recoveryContentBytes(): number { return 32_768 }
  render(): unknown { return this.rendered }
  drainNewMessages(): unknown[] { return this.messages }
  preservedRefs(): string[] { return [] }
}

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
