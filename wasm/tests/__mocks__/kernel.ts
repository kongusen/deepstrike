// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export class KernelRuntime {
  private terminal = false
  private phase = 0
  private maxTurns: number
  private rendered = { systemText: "", turns: [] as unknown[] }
  private messages: unknown[] = []
  private governanceAskUser = false
  private resumedAfterAsk = false

  constructor(policy: { maxTokens: number; maxTurns?: number }) {
    this.maxTurns = policy.maxTurns ?? 25
  }

  step(inputJson: string): string {
    const input = JSON.parse(inputJson) as { event?: Record<string, unknown> }
    const event = input.event ?? {}
    const actions: Array<Record<string, unknown>> = []
    const observations: Array<Record<string, unknown>> = []

    switch (event.kind) {
      case "load_governance_policy": {
        const rules = (event.rules as Array<{ action?: string }>) ?? []
        this.governanceAskUser = rules.some(r => r.action === "ask_user")
        break
      }
      case "set_attention_policy":
        break
      case "start_run":
        this.phase = 0
        this.terminal = false
        this.resumedAfterAsk = false
        this.rendered = { systemText: "", turns: [{ role: "user", content: "test" }] }
        actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
        break
      case "resume": {
        this.resumedAfterAsk = true
        const approved = (event.approved_calls as string[]) ?? []
        if (approved.length > 0) {
          actions.push({
            kind: "execute_tool",
            calls: [{ id: approved[0], name: "needs_approval", arguments: "{}" }],
          })
        } else {
          this.rendered = { systemText: "", turns: [{ role: "user", content: "resume" }] }
          actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
        }
        break
      }
      case "provider_result": {
        const message = (event.message as Record<string, unknown>) ?? {}
        this.messages.push(message)
        const toolCalls = (message.tool_calls as Array<{ id?: string; name?: string }>) ?? []
        if (this.phase === 0 && toolCalls.length > 0 && this.governanceAskUser && !this.resumedAfterAsk) {
          const call = toolCalls[0]
          observations.push(
            {
              kind: "tool_gated",
              turn: 1,
              call_id: call.id ?? "c1",
              tool: call.name ?? "needs_approval",
              reason: "ask_user",
            },
            {
              kind: "suspended",
              turn: 1,
              reason: "ask_user",
              pending_calls: [call.id ?? "c1"],
            },
          )
          break
        }
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
      case "spawn_sub_agent":
        return JSON.stringify({
          version: 1,
          actions: [],
          observations: [
            {
              kind: "agent_process_changed",
              turn: 1,
              agent_id: "worker",
              parent_session_id: "parent-session-001",
              role: "implement",
              isolation: "shared",
              context_inheritance: "full",
              state: "running",
              permitted_capability_ids: ["read_file"],
            },
            { kind: "suspended", turn: 1, reason: "sub_agent_await", pending_calls: ["worker"] },
          ],
        })
      default:
        break
    }

    return JSON.stringify({ version: 1, actions, observations })
  }

  isTerminal(): boolean { return this.terminal }
  turn(): number { return this.phase }
  recoveryContentBytes(): number { return 32_768 }
  render(): unknown { return this.rendered }
  drainNewMessages(): unknown[] { return this.messages }
  preservedRefs(): string[] { return [] }
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
