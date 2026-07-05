// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export const kernelEvents: Record<string, unknown>[] = []

export class KernelRuntime {
  private terminal = false
  private phase = 0
  private maxTurns: number
  private rendered = { systemText: "", turns: [] as unknown[] }
  private messages: unknown[] = []
  private governanceAskUser = false
  private resumedAfterAsk = false
  // Mirrors the real kernel's bounded reactive-recovery ladder (see eviction.rs
  // MAX_RECOVERY_ATTEMPTS): compact-and-retry up to the cap, then terminate ContextOverflow.
  private recoveryAttempts = 0
  // ③ loop-agent pacing trap (DW-3): armed by `start_run.run_spec.loop_round`. A `pace` tool call
  // is trapped in-kernel (never forwarded to the host plane); the adjudicated decision rides the
  // done result as `pace_decision`. Silence = the spec's default_action ("stop" = CC contract).
  private loopRound: { default_action?: string } | null = null
  private paceProposal: { action: string; reason: string } | null = null

  constructor(policy: { maxTokens: number; maxTurns?: number }) {
    this.maxTurns = policy.maxTurns ?? 25
  }

  step(inputJson: string): string {
    const input = JSON.parse(inputJson) as { event?: Record<string, unknown> }
    const event = input.event ?? {}
    kernelEvents.push(event)
    const actions: Array<Record<string, unknown>> = []
    const observations: Array<Record<string, unknown>> = []

    switch (event.kind) {
      case "load_governance_policy": {
        const rules = (event.rules as Array<{ action?: string }>) ?? []
        this.governanceAskUser = rules.some(r => r.action === "ask_user")
        break
      }
      case "configure_run": {
        // K2: the SDK now bundles governance (+ attention/scheduler/quota — no-ops in this mock) into
        // one event. Apply governance the same way `load_governance_policy` does.
        const config = (event.config as Record<string, unknown>) ?? {}
        const governance = (config.governance as { rules?: Array<{ action?: string }> }) ?? {}
        const rules = governance.rules ?? []
        this.governanceAskUser = rules.some(r => r.action === "ask_user")
        break
      }
      case "set_attention_policy":
        break
      case "start_run":
        this.phase = 0
        this.terminal = false
        this.resumedAfterAsk = false
        // DW-3: arm the pacing trap when the run spec carries `loop_round` (loop-node iterations).
        this.loopRound = ((event.run_spec as { loop_round?: { default_action?: string } } | undefined)?.loop_round) ?? null
        this.paceProposal = null
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
      case "provider_error": {
        // Reactive recovery mirror of the real kernel: classify the error, compact-and-retry on a
        // bounded overflow ladder, else terminate with an honest reason.
        const msg = String(event.message ?? "").toLowerCase()
        const isOverflow =
          msg.includes("413") || msg.includes("too long") ||
          msg.includes("context length exceeded") || msg.includes("context_length_exceeded")
        if (!isOverflow) {
          this.terminal = true
          actions.push({ kind: "done", result: { turns_used: this.turn(), total_tokens_used: 0, termination: "error" } })
        } else if (this.recoveryAttempts >= 2) {
          this.terminal = true
          actions.push({ kind: "done", result: { turns_used: this.turn(), total_tokens_used: 0, termination: "context_overflow" } })
        } else {
          this.recoveryAttempts += 1
          observations.push({ kind: "compressed", action: "auto_compact", rho_after: 0.4, summary: null, archived: [] })
          this.rendered = { systemText: "", turns: [{ role: "user", content: "retry" }] }
          actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
        }
        break
      }
      case "provider_result": {
        const message = (event.message as Record<string, unknown>) ?? {}
        this.messages.push(message)
        // A response arrived ⇒ the prompt fit ⇒ reset the overflow recovery ladder.
        this.recoveryAttempts = 0
        const toolCalls = (message.tool_calls as Array<{ id?: string; name?: string; arguments?: unknown }>) ?? []
        // ③ pacing trap: a `pace` call on an armed run is adjudicated in-kernel — record the
        // proposal and resume the reason loop; the verb never reaches the host execution plane.
        const paceCall = this.loopRound ? toolCalls.find(tc => tc.name === "pace") : undefined
        if (paceCall) {
          const rawArgs = paceCall.arguments
          const args = (typeof rawArgs === "string" ? JSON.parse(rawArgs || "{}") : rawArgs ?? {}) as { next?: string; reason?: string }
          this.paceProposal = { action: args.next ?? "stop", reason: args.reason ?? "" }
          this.rendered = { systemText: "", turns: [{ role: "user", content: "paced" }] }
          actions.push({ kind: "call_provider", context: this.rendered, tools: [] })
          break
        }
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
            result: {
              turns_used: 2,
              total_tokens_used: 100,
              termination: "completed",
              // ③ armed run: the adjudicated pace decision rides the done result. Silence = the
              // default action (stop for loop-node iterations — the CC silence-is-done contract).
              ...(this.loopRound
                ? {
                    pace_decision: this.paceProposal
                      ? { action: this.paceProposal.action, reason: this.paceProposal.reason }
                      : { action: this.loopRound.default_action ?? "stop", reason: "no pace call (default)" },
                  }
                : {}),
            },
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

// Eval / harness quality gate (0.5.0 fold: free functions, was the EvalPipeline class).
export function buildEvalMessages(
  _goal: string, _criteria: unknown[], _result: string, _attempt: number, _extractSkillOnPass: boolean,
) {
  return []
}

export function parseVerdict(_content: string) {
  return { passed: true, overallScore: 1, feedback: "", details: [], skillCandidate: undefined }
}

export function verdictOutputSchema(extractSkillOnPass: boolean) {
  const properties: Record<string, unknown> = {
    passed: { type: "boolean" },
    overall_score: { type: "number" },
    feedback: { type: "string" },
    details: { type: "array" },
  }
  if (extractSkillOnPass) properties.skill = { type: "object" }
  return JSON.stringify({ type: "object", required: ["passed", "overall_score", "feedback"], properties })
}
