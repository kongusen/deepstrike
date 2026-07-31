// Mock @deepstrike/wasm-kernel for tests (no .wasm binary needed)
export const kernelEvents: Record<string, unknown>[] = []

export function kernelAbiVersion(): number {
  return 3
}

type CanonicalLifecycle =
  | "created"
  | "configured"
  | "running"
  | "suspended"
  | "completed"
  | "cancelled"
  | "failed"

/**
 * Minimal ABI-v3 CanonicalKernel stand-in for Jest (no .wasm). Mirrors the legacy KernelRuntime
 * mock's agent loop so existing runner tests keep working after Task 21 cutover.
 */
export class CanonicalKernel {
  private life: CanonicalLifecycle = "created"
  private nextStep = 0
  private head: string | undefined
  private nextEffect = 1
  private phase = 0
  private terminalPayload: Record<string, unknown> | undefined
  private pendingEffects: Array<Record<string, unknown>> = []
  private prepared = new Map<string, {
    stepSeq: number
    recordDigest: string
    plannedStepJson: string
    expectedHead?: string
  }>()
  private governanceAskUser = false
  private resumedAfterAsk = false
  private recoveryAttempts = 0
  private loopRound: { default_action?: string } | null = null
  private paceProposal: { action: string; reason: string } | null = null
  private turns = 0
  private checkpointThrough = 0

  private effect(kind: string, payload: Record<string, unknown> = {}): Record<string, unknown> {
    return { effect_id: `mock-effect-${this.nextEffect++}`, effect: { kind, ...payload } }
  }

  private planned(
    effects: Array<Record<string, unknown>> = [],
    observations: Array<Record<string, unknown>> = [],
    terminal?: Record<string, unknown>,
  ): string {
    return JSON.stringify({
      observations,
      disposition: terminal
        ? { kind: "terminal", terminal }
        : { kind: "effects", effects },
    })
  }

  private providerContext() {
    return {
      system_text: "",
      turns: [{ role: "user", content: "test" }],
      tools: [],
    }
  }

  prepare(inputJson: string): {
    status: "prepared" | "replayed" | "rejected"
    prepareToken?: string
    stepSeq?: string
    expectedHead?: string
    recordDigest?: string
    recordBytes?: Uint8Array
    plannedStepJson?: string
    faultJson?: string
  } {
    let envelope: Record<string, unknown>
    try {
      envelope = JSON.parse(inputJson) as Record<string, unknown>
    } catch {
      return { status: "rejected", faultJson: JSON.stringify({ code: "invalid_json", message: "bad envelope" }) }
    }
    const input = (envelope.input && typeof envelope.input === "object"
      ? envelope.input
      : {}) as Record<string, unknown>
    kernelEvents.push(input)

    let plannedStepJson = this.planned()
    let observations: Array<Record<string, unknown>> = []

    switch (String(input.kind ?? "")) {
      case "configure_operation": {
        const config = (input.config && typeof input.config === "object"
          ? input.config
          : {}) as Record<string, unknown>
        // Canonical lowering stores governance as `governance_policy`; legacy hosts used `governance`.
        const governance = (
          (config.governance_policy && typeof config.governance_policy === "object"
            ? config.governance_policy
            : config.governance && typeof config.governance === "object"
              ? config.governance
              : {})
        ) as { rules?: Array<{ action?: string }> }
        this.governanceAskUser = (governance.rules ?? []).some(rule => rule.action === "ask_user")
        this.life = "configured"
        break
      }
      case "start_operation": {
        const entry = (input.entry && typeof input.entry === "object" ? input.entry : {}) as Record<string, unknown>
        this.phase = 0
        this.terminalPayload = undefined
        this.resumedAfterAsk = false
        this.recoveryAttempts = 0
        this.paceProposal = null
        const runSpec = (entry.run_spec && typeof entry.run_spec === "object"
          ? entry.run_spec
          : {}) as { loop_round?: { default_action?: string } }
        this.loopRound = runSpec.loop_round ?? null
        this.life = "running"
        if (entry.kind === "workflow") {
          // Workflow roots complete immediately in this mock unless later resolve_effect advances them.
          this.life = "completed"
          this.terminalPayload = {
            kind: "workflow",
            outcome: { status: "completed" },
            usage: { turns: 0, input_tokens: 0, output_tokens: 0 },
          }
          plannedStepJson = this.planned([], [], this.terminalPayload)
        } else {
          plannedStepJson = this.planned([
            this.effect("call_provider", { context: this.providerContext(), tools: [] }),
          ])
        }
        break
      }
      case "resolve_effect": {
        const outcome = (input.outcome && typeof input.outcome === "object"
          ? input.outcome
          : {}) as Record<string, unknown>
        if (outcome.status === "failed") {
          const failure = (outcome.failure && typeof outcome.failure === "object"
            ? outcome.failure
            : {}) as Record<string, unknown>
          this.life = "failed"
          this.terminalPayload = {
            kind: "failed",
            failure: { code: "provider_error", message: String(failure.message ?? "error") },
            usage: { turns: this.turns, input_tokens: 0, output_tokens: 0 },
          }
          plannedStepJson = this.planned([], [], this.terminalPayload)
          break
        }
        const result = (outcome.result && typeof outcome.result === "object"
          ? outcome.result
          : {}) as Record<string, unknown>
        if (result.kind === "provider") {
          const providerOutcome = (result.outcome && typeof result.outcome === "object"
            ? result.outcome
            : {}) as Record<string, unknown>
          // Canonical overflow path: succeeded + provider.outcome.context_overflow
          if (providerOutcome.kind === "context_overflow") {
            if (this.recoveryAttempts >= 2) {
              this.life = "failed"
              this.terminalPayload = {
                kind: "failed",
                failure: { code: "provider_recovery_exhausted", message: "context overflow" },
                usage: { turns: this.turns, input_tokens: 0, output_tokens: 0 },
              }
              plannedStepJson = this.planned([], [], this.terminalPayload)
            } else {
              this.recoveryAttempts += 1
              observations = [{ kind: "compressed", action: "auto_compact", rho_after: 0.4, summary: null, archived_count: 0 }]
              plannedStepJson = this.planned([
                this.effect("call_provider", { context: this.providerContext(), tools: [] }),
              ], observations)
            }
            break
          }
          const message = (providerOutcome.message && typeof providerOutcome.message === "object"
            ? providerOutcome.message
            : {}) as Record<string, unknown>
          this.recoveryAttempts = 0
          this.turns += 1
          const toolCalls = (Array.isArray(message.tool_calls) ? message.tool_calls : []) as Array<{
            call_id?: string; id?: string; name?: string; arguments?: unknown
          }>
          const paceCall = this.loopRound ? toolCalls.find(tc => tc.name === "pace") : undefined
          if (paceCall) {
            const rawArgs = paceCall.arguments
            const args = (typeof rawArgs === "string" ? JSON.parse(rawArgs || "{}") : rawArgs ?? {}) as {
              next?: string; reason?: string
            }
            this.paceProposal = { action: args.next ?? "stop", reason: args.reason ?? "" }
            plannedStepJson = this.planned([
              this.effect("call_provider", { context: this.providerContext(), tools: [] }),
            ])
            break
          }
          if (this.phase === 0 && toolCalls.length > 0 && this.governanceAskUser && !this.resumedAfterAsk) {
            const call = toolCalls[0]
            plannedStepJson = this.planned([
              this.effect("request_approval", {
                requests: [{
                  call_id: call.call_id ?? call.id ?? "c1",
                  tool_name: call.name ?? "needs_approval",
                  arguments: call.arguments ?? {},
                  reason: "ask_user",
                }],
              }),
            ])
            break
          }
          if (this.phase === 0 && toolCalls.length > 0) {
            this.phase = 1
            plannedStepJson = this.planned([
              this.effect("execute_tools", {
                calls: toolCalls.map(call => ({
                  call_id: call.call_id ?? call.id ?? "c1",
                  name: call.name ?? "",
                  arguments: call.arguments ?? {},
                })),
              }),
            ])
          } else {
            this.life = "completed"
            this.terminalPayload = {
              kind: "agent",
              result: {
                termination: "completed",
                turns_used: Math.max(2, this.turns),
                final_message: {
                  role: "assistant",
                  content: String(message.content ?? ""),
                },
                ...(this.loopRound
                  ? {
                      pace_decision: this.paceProposal
                        ? { action: this.paceProposal.action, reason: this.paceProposal.reason }
                        : { action: this.loopRound.default_action ?? "stop", reason: "no pace call (default)" },
                    }
                  : {}),
              },
              usage: { turns: Math.max(2, this.turns), input_tokens: 50, output_tokens: 50 },
            }
            plannedStepJson = this.planned([], [], this.terminalPayload)
          }
          break
        }
        if (result.kind === "tools" || result.kind === "approval") {
          if (result.kind === "approval") this.resumedAfterAsk = true
          const approved = (result.approved_call_ids as string[] | undefined) ?? []
          if (result.kind === "approval" && approved.length > 0) {
            plannedStepJson = this.planned([
              this.effect("execute_tools", {
                calls: [{ call_id: approved[0], name: "needs_approval", arguments: {} }],
              }),
            ])
          } else {
            plannedStepJson = this.planned([
              this.effect("call_provider", { context: this.providerContext(), tools: [] }),
            ])
          }
          break
        }
        // Default: keep the loop alive with another provider call.
        plannedStepJson = this.planned([
          this.effect("call_provider", { context: this.providerContext(), tools: [] }),
        ])
        break
      }
      case "host_control": {
        const command = (input.command && typeof input.command === "object"
          ? input.command
          : {}) as Record<string, unknown>
        if (command.kind === "cancel") {
          this.life = "cancelled"
          observations = [{
            kind: "operation_cancelled",
            turn: this.turns,
            reason: command.reason,
            pending_call_ids: command.pending_call_ids ?? [],
          }]
          this.terminalPayload = {
            kind: "cancelled",
            reason: String(command.reason ?? "user"),
            usage: { turns: this.turns, input_tokens: 0, output_tokens: 0 },
          }
          plannedStepJson = this.planned([], observations, this.terminalPayload)
        }
        break
      }
      case "deliver_external_event":
        break
      default:
        break
    }

    const stepSeq = this.nextStep
    const recordDigest = `digest-${stepSeq}`
    const prepareToken = `token-${stepSeq}`
    const plannedParsed = JSON.parse(plannedStepJson) as {
      disposition: { kind: string; effects?: Array<Record<string, unknown>> }
    }
    this.pendingEffects = plannedParsed.disposition.kind === "effects"
      ? (plannedParsed.disposition.effects ?? [])
      : []
    this.prepared.set(prepareToken, {
      stepSeq,
      recordDigest,
      plannedStepJson,
      expectedHead: this.head,
    })
    return {
      status: "prepared",
      prepareToken,
      stepSeq: String(stepSeq),
      ...(this.head ? { expectedHead: this.head } : {}),
      recordDigest,
      recordBytes: new TextEncoder().encode(`record:${inputJson}`),
      plannedStepJson,
    }
  }

  commit(prepareToken: string, appendedHead: string): {
    stepSeq: string
    recordDigest: string
    plannedStepJson: string
    checkpointAdviceJson?: string
  } {
    const prepared = this.prepared.get(prepareToken)
    if (!prepared) throw new Error(`unknown prepare token ${prepareToken}`)
    this.prepared.delete(prepareToken)
    this.head = appendedHead
    this.checkpointThrough = prepared.stepSeq
    this.nextStep = prepared.stepSeq + 1
    const planned = JSON.parse(prepared.plannedStepJson) as {
      disposition: { kind: string; effects?: Array<Record<string, unknown>>; terminal?: Record<string, unknown> }
    }
    this.pendingEffects = planned.disposition.kind === "effects"
      ? (planned.disposition.effects ?? [])
      : []
    if (planned.disposition.kind === "terminal") {
      this.terminalPayload = planned.disposition.terminal
    }
    return {
      stepSeq: String(prepared.stepSeq),
      recordDigest: prepared.recordDigest,
      plannedStepJson: prepared.plannedStepJson,
    }
  }

  abort(prepareToken: string): void {
    this.prepared.delete(prepareToken)
  }

  checkpointCandidate() {
    return {
      checkpointBytes: new TextEncoder().encode(`checkpoint-${this.checkpointThrough}`),
      throughStepSeq: String(this.checkpointThrough),
      coveredHead: this.head ?? `digest-${this.checkpointThrough}`,
      stateDigest: `state-${this.checkpointThrough}`,
      ackToken: `checkpoint-${this.checkpointThrough}`,
    }
  }

  checkpointRebase(_checkpointBytes: Uint8Array) {
    return this.checkpointCandidate()
  }

  ackCheckpoint(_throughStepSeq: string, _coveredHead: string): void {}

  restore(_checkpointBytes: Uint8Array | null | undefined, recordBytes: Uint8Array[]) {
    this.nextStep = recordBytes.length
    this.head = recordBytes.length ? `digest-${recordBytes.length - 1}` : undefined
    this.life = recordBytes.length ? "running" : "created"
    this.pendingEffects = []
    this.terminalPayload = undefined
    return {
      recordsBeforeCheckpoint: "0",
      tailInputsReplayed: String(recordBytes.length),
      recordsAfterCheckpoint: String(recordBytes.length),
      bytesRead: String(recordBytes.reduce((n, bytes) => n + bytes.byteLength, 0)),
    }
  }

  lifecycle(): CanonicalLifecycle {
    return this.life
  }

  pendingEffectsJson(): string {
    return JSON.stringify(this.pendingEffects)
  }

  terminalJson(): string | undefined {
    return this.terminalPayload ? JSON.stringify(this.terminalPayload) : undefined
  }
}

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
  private nextEffect = 1
  private operationId: string | undefined
  private acceptedInputs: Array<Record<string, unknown>> = []

  private effect(kind: string, payload: Record<string, unknown>): Record<string, unknown> {
    return { effect_id: `mock-effect-${this.nextEffect++}`, kind, ...payload }
  }

  constructor(policy: { maxTokens: number; maxTurns?: number; maxTotalTokens?: number; timeoutMs?: number }) {
    if (policy.timeoutMs !== undefined && typeof policy.timeoutMs !== "number") {
      throw new TypeError("WASM LoopPolicy.timeoutMs must be a number")
    }
    this.maxTurns = policy.maxTurns ?? 25
  }

  step(inputJson: string): string {
    const input = JSON.parse(inputJson) as { event?: Record<string, unknown> }
    const envelope = JSON.parse(inputJson) as Record<string, unknown>
    this.operationId = String(envelope.operation_id ?? this.operationId ?? "") || undefined
    this.acceptedInputs.push(envelope)
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
      case "start_run":
        this.phase = 0
        this.terminal = false
        this.resumedAfterAsk = false
        // DW-3: arm the pacing trap when the run spec carries `loop_round` (loop-node iterations).
        this.loopRound = ((event.run_spec as { loop_round?: { default_action?: string } } | undefined)?.loop_round) ?? null
        this.paceProposal = null
        this.rendered = { systemText: "", turns: [{ role: "user", content: "test" }] }
        actions.push(this.effect("call_provider", { context: this.rendered, tools: [] }))
        break
      case "approval_result": {
        this.resumedAfterAsk = true
        const approved = (event.approved_calls as string[]) ?? []
        if (approved.length > 0) {
          actions.push(this.effect("execute_tool", {
            calls: [{ id: approved[0], name: "needs_approval", arguments: "{}" }],
          }))
        } else {
          this.rendered = { systemText: "", turns: [{ role: "user", content: "resume" }] }
          actions.push(this.effect("call_provider", { context: this.rendered, tools: [] }))
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
          actions.push(this.effect("done", { result: { turns_used: this.turn(), total_tokens_used: 0, termination: "error" } }))
        } else if (this.recoveryAttempts >= 2) {
          this.terminal = true
          actions.push(this.effect("done", { result: { turns_used: this.turn(), total_tokens_used: 0, termination: "context_overflow" } }))
        } else {
          this.recoveryAttempts += 1
          observations.push({ kind: "compressed", action: "auto_compact", rho_after: 0.4, summary: null, archived_count: 0 })
          this.rendered = { systemText: "", turns: [{ role: "user", content: "retry" }] }
          actions.push(this.effect("call_provider", { context: this.rendered, tools: [] }))
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
          actions.push(this.effect("call_provider", { context: this.rendered, tools: [] }))
          break
        }
        if (this.phase === 0 && toolCalls.length > 0 && this.governanceAskUser && !this.resumedAfterAsk) {
          const call = toolCalls[0]
          actions.push(this.effect("request_approval", { requests: [{
            call_id: call.id ?? "c1", tool: call.name ?? "needs_approval",
            arguments: call.arguments ?? {}, reason: "ask_user",
          }] }))
          break
        }
        if (this.phase === 0 && toolCalls.length > 0) {
          this.phase = 1
          actions.push(this.effect("execute_tool", { calls: toolCalls }))
        } else {
          this.terminal = true
          actions.push(this.effect("done", {
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
          }))
        }
        break
      }
      case "tool_results":
        actions.push(this.effect("call_provider", { context: { systemText: "", turns: [] }, tools: [] }))
        break
      case "cancel_operation":
        this.terminal = true
        observations.push({
          kind: "operation_cancelled",
          turn: this.turn(),
          operation_id: event.operation_id,
          reason: event.reason,
          pending_call_ids: event.pending_call_ids ?? [],
        })
        actions.push(this.effect("done", {
          result: { turns_used: this.turn(), total_tokens_used: 0, termination: "user_abort" },
        }))
        break
      case "spawn_sub_agent":
        return JSON.stringify({
          version: 2,
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

    return JSON.stringify({ version: 2, actions, observations, faults: [] })
  }

  isTerminal(): boolean { return this.terminal }
  snapshot(): string {
    return JSON.stringify({
      snapshot_version: 2,
      abi_version: 2,
      initial_policy: {},
      lifecycle: this.terminal ? "completed" : (this.operationId ? "running" : "created"),
      operation_id: this.operationId,
      next_step_seq: this.nextEffect,
      snapshot_input_limit: 10_000,
      max_input_bytes: 16 * 1024 * 1024,
      snapshot_journal_bytes_limit: 64 * 1024 * 1024,
      accepted_input_bytes: JSON.stringify(this.acceptedInputs).length,
      accepted_inputs: this.acceptedInputs,
    })
  }
  diagnostics(): string {
    return JSON.stringify({
      lifecycle: this.terminal ? "completed" : (this.operationId ? "running" : "created"),
      next_step_seq: this.nextEffect,
      accepted_input_count: this.acceptedInputs.length,
      accepted_input_bytes: JSON.stringify(this.acceptedInputs).length,
      snapshot_input_limit: 10_000,
      snapshot_journal_bytes_limit: 64 * 1024 * 1024,
      max_input_bytes: 16 * 1024 * 1024,
      snapshot_overflowed: false,
      recorded_event_count: this.acceptedInputs.length,
      completed_effect_count: 0,
      pending_effect_count: 0,
    })
  }
  restore(snapshotJson: string): void {
    const snapshot = JSON.parse(snapshotJson) as { operation_id?: string; lifecycle?: string; accepted_inputs?: Array<Record<string, unknown>> }
    this.operationId = snapshot.operation_id
    this.terminal = ["completed", "cancelled", "failed"].includes(snapshot.lifecycle ?? "")
    this.acceptedInputs = snapshot.accepted_inputs ?? []
  }
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
  ingest(signal: unknown, lifecycle: "ready" | "running" | "suspended" | "done"): string {
    if (!["ready", "running", "suspended", "done"].includes(lifecycle)) {
      throw new TypeError(`invalid task lifecycle ${String(lifecycle)}`)
    }
    if (lifecycle === "done") return "queue"
    const urgency = (signal as { urgency?: string }).urgency
    if (urgency === "critical") {
      return lifecycle === "ready" ? "run" : "interrupt_now"
    }
    if (urgency === "high") {
      return lifecycle === "ready" ? "run" : "interrupt"
    }
    if (urgency === "normal") return lifecycle === "ready" ? "run" : "queue"
    return "observe"
  }
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
