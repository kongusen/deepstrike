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
 * Minimal CanonicalKernel stand-in for Jest (no .wasm). It models the agent loop used by runner
 * tests while the real binding contract remains covered by the wasm-pack smoke test.
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
  private workflowNodes: Array<Record<string, unknown>> = []
  private workflowStarted = new Set<string>()
  private workflowCompleted = new Set<string>()

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

  private readyWorkflowTasks(): Array<Record<string, unknown>> {
    const ready = this.workflowNodes.filter(node => {
      const nodeId = String(node.node_id ?? "")
      const dependencies = Array.isArray(node.depends_on) ? node.depends_on.map(String) : []
      return !this.workflowStarted.has(nodeId)
        && dependencies.every(dependency => this.workflowCompleted.has(dependency))
    })
    for (const node of ready) this.workflowStarted.add(String(node.node_id ?? ""))
    return ready.map(node => {
      const nodeId = String(node.node_id ?? "")
      const task = (node.task && typeof node.task === "object" ? node.task : {}) as Record<string, unknown>
      const runSpec = (node.run_spec && typeof node.run_spec === "object"
        ? node.run_spec
        : { goal: task.goal ?? "" }) as Record<string, unknown>
      return {
        task_id: nodeId,
        attempt_id: `${nodeId}:attempt:1`,
        launch_token: `${nodeId}:launch:1`,
        node_id: nodeId,
        spec: runSpec,
      }
    })
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
          const spec = (entry.spec && typeof entry.spec === "object" ? entry.spec : {}) as Record<string, unknown>
          this.workflowNodes = Array.isArray(spec.nodes)
            ? spec.nodes.filter(node => node && typeof node === "object") as Array<Record<string, unknown>>
            : []
          this.workflowStarted = new Set()
          this.workflowCompleted = new Set()
          const tasks = this.readyWorkflowTasks()
          plannedStepJson = this.planned(tasks.length > 0 ? [this.effect("spawn_tasks", { tasks })] : [])
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
        if (result.kind === "tasks_spawned") {
          plannedStepJson = this.planned()
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
      case "deliver_external_event": {
        const event = (input.event && typeof input.event === "object"
          ? input.event
          : {}) as Record<string, unknown>
        if (event.kind !== "child_completed") break
        this.workflowCompleted.add(String(event.task_id ?? ""))
        const tasks = this.readyWorkflowTasks()
        if (tasks.length > 0) {
          plannedStepJson = this.planned([this.effect("spawn_tasks", { tasks })])
          break
        }
        if (this.workflowCompleted.size === this.workflowNodes.length) {
          this.life = "completed"
          observations = [{
            kind: "workflow_completed",
            node_outcomes: this.workflowNodes.map(node => ({
              node_id: String(node.node_id ?? ""),
              status: "completed",
              termination: "completed",
            })),
          }]
          this.terminalPayload = {
            kind: "workflow",
            outcome: { status: "completed" },
            usage: { turns: 0, input_tokens: 0, output_tokens: 0 },
          }
          plannedStepJson = this.planned([], observations, this.terminalPayload)
        }
        break
      }
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
      recordBytes: new TextEncoder().encode(JSON.stringify({
        inputJson,
        plannedStepJson,
        recordDigest,
      })),
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
    this.pendingEffects = []
    this.terminalPayload = undefined
    this.life = "created"
    this.head = undefined
    if (recordBytes.length > 0) {
      const restored = JSON.parse(new TextDecoder().decode(recordBytes[recordBytes.length - 1])) as {
        inputJson: string
        plannedStepJson: string
        recordDigest: string
      }
      const input = (JSON.parse(restored.inputJson) as { input?: { kind?: string } }).input ?? {}
      const planned = JSON.parse(restored.plannedStepJson) as {
        disposition: {
          kind: string
          effects?: Array<Record<string, unknown>>
          terminal?: Record<string, unknown>
        }
      }
      this.head = restored.recordDigest
      if (planned.disposition.kind === "terminal") {
        this.terminalPayload = planned.disposition.terminal
        const kind = String(planned.disposition.terminal?.kind ?? "completed")
        this.life = kind === "cancelled" ? "cancelled" : kind === "failed" ? "failed" : "completed"
      } else {
        this.pendingEffects = planned.disposition.effects ?? []
        this.life = input.kind === "configure_operation" ? "configured" : "running"
      }
    }
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
