import type {
  CanonicalKernel as CanonicalKernelInstance,
  CanonicalPreparation,
  CanonicalRestoreCost,
} from "@deepstrike/wasm-kernel"
import { kernelAbiVersion } from "@deepstrike/wasm-kernel"
import type { Message } from "../types.js"
import {
  JournalCasConflictError,
  MAX_CHAIN_POSITION as JOURNAL_MAX_CHAIN_POSITION,
  type InstalledCheckpoint,
  type KernelJournal,
} from "./kernel-journal.js"
import {
  encodeCanonicalContentParts,
  kernelMessageToSdk,
  renderedContextToSdk,
  type KernelObservation,
  type KernelRunnerAction,
} from "./kernel-step.js"

export const MAX_CHAIN_POSITION = JOURNAL_MAX_CHAIN_POSITION

const utf8ByteLength = (value: string): number => new TextEncoder().encode(value).byteLength

export type CanonicalKernelInput = Record<string, unknown> & { kind: string }

export interface CanonicalPlannedStep {
  root_kind?: "agent" | "workflow"
  focus?: Record<string, unknown>
  observations?: Array<Record<string, unknown>>
  disposition:
    | { kind: "effects"; effects?: Array<Record<string, unknown>> }
    | { kind: "terminal"; terminal: Record<string, unknown> }
}

export interface CanonicalTransitionOptions {
  /** Caller-stable idempotency key. Generated once when omitted. */
  inputId?: string
  /** Decimal-u64 host observation. Captured once when omitted and preserved across every retry. */
  observedAtMs?: string
}

export interface CanonicalTransition {
  inputJson: string
  stepSeq: number
  recordDigest: string
  plannedStep: CanonicalPlannedStep
  checkpointAdvice?: Record<string, unknown>
  replayed: boolean
}

function asObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? value as Record<string, unknown> : {}
}

function totalUsageTokens(terminal: Record<string, unknown>): number {
  const usage = asObject(terminal.usage)
  const input = Number(usage.input_tokens ?? 0)
  const output = Number(usage.output_tokens ?? 0)
  return Number.isSafeInteger(input + output) ? input + output : 0
}

export function canonicalUnsupportedEffectResolution(
  effectId: string,
  effectKind: string,
): CanonicalKernelInput {
  return {
    kind: "resolve_effect",
    effect_id: effectId,
    outcome: {
      status: "failed",
      failure: {
        kind: "protocol_error",
        message: `unknown canonical effect kind: ${effectKind}`,
        retryable: false,
      },
    },
  }
}

/** The only ABI-v3 planned-step → Node host-action projection. */
export function canonicalActionFromPlannedStep(
  plannedStep: CanonicalPlannedStep,
): KernelRunnerAction | null {
  if (plannedStep.disposition.kind === "terminal") {
    const terminal = plannedStep.disposition.terminal
    const usage = asObject(terminal.usage)
    let termination = String(terminal.kind ?? "failed")
    let turnsUsed = Number(usage.turns ?? 0)
    if (terminal.kind === "agent") {
      const result = asObject(terminal.result)
      termination = String(result.termination ?? "completed")
      turnsUsed = Number(result.turns_used ?? turnsUsed)
      const finalMessage = asObject(result.final_message)
      const pace = asObject(result.pace_decision)
      if (Object.keys(finalMessage).length > 0) {
        return {
          kind: "done",
          effectId: "",
          result: {
            termination,
            turnsUsed,
            totalTokensUsed: totalUsageTokens(terminal),
            finalMessage: {
              role: String(finalMessage.role ?? "assistant") as Message["role"],
              content: String(finalMessage.content ?? ""),
              toolCalls: (Array.isArray(finalMessage.tool_calls) ? finalMessage.tool_calls : [])
                .map(value => {
                  const call = asObject(value)
                  return {
                    id: String(call.call_id ?? ""),
                    name: String(call.name ?? ""),
                    arguments: JSON.stringify(call.arguments ?? {}),
                  }
                }),
            },
            ...(Object.keys(pace).length > 0
              ? {
                  paceDecision: {
                    action: String(pace.action ?? "stop") as "continue" | "sleep" | "stop",
                    ...(pace.delay_ms !== undefined ? { delayMs: Number(pace.delay_ms) } : {}),
                    reason: String(pace.reason ?? ""),
                    ...(pace.coerced_from ? { coercedFrom: String(pace.coerced_from) } : {}),
                  },
                }
              : {}),
          },
        }
      }
    } else if (terminal.kind === "workflow") {
      const outcome = asObject(terminal.outcome)
      termination = String(outcome.status ?? "completed")
    } else if (terminal.kind === "cancelled") {
      termination = String(terminal.reason ?? "cancelled")
    } else if (terminal.kind === "failed") {
      const failure = asObject(terminal.failure)
      termination = failure.code === "provider_recovery_exhausted"
        ? "context_overflow"
        : "error"
    }
    return {
      kind: "done",
      effectId: "",
      result: {
        termination,
        turnsUsed,
        totalTokensUsed: totalUsageTokens(terminal),
      },
    }
  }

  const published = plannedStep.disposition.effects ?? []
  if (published.length === 0) return null
  if (published.length !== 1) {
    throw new Error(`WASM runner expects one canonical effect at a time, received ${published.length}`)
  }
  const envelope = asObject(published[0])
  const effectId = String(envelope.effect_id ?? "")
  const effect = asObject(envelope.effect)
  if (!effectId) throw new Error("canonical effect is missing effect_id")

  switch (effect.kind) {
    case "call_provider":
      return {
        kind: "call_provider",
        effectId,
        context: renderedContextToSdk(asObject(effect.context)),
        tools: (Array.isArray(effect.tools) ? effect.tools : []).map(raw => {
          const tool = asObject(raw)
          return {
            name: String(tool.name ?? ""),
            description: String(tool.description ?? ""),
            parameters: JSON.stringify(tool.parameters ?? {}),
          }
        }),
      }
    case "execute_tools":
      return {
        kind: "execute_tool",
        effectId,
        calls: (Array.isArray(effect.calls) ? effect.calls : []).map(raw => {
          const call = asObject(raw)
          return {
            id: String(call.call_id ?? ""),
            name: String(call.name ?? ""),
            arguments: JSON.stringify(call.arguments ?? {}),
          }
        }),
      }
    case "request_approval":
      return {
        kind: "request_approval",
        effectId,
        requests: (Array.isArray(effect.requests) ? effect.requests : []).map(raw => {
          const request = asObject(raw)
          return {
            callId: String(request.call_id ?? ""),
            tool: String(request.tool_name ?? ""),
            arguments: JSON.stringify(request.arguments ?? {}),
            reason: String(request.reason ?? ""),
          }
        }),
      }
    case "spawn_tasks":
      return {
        kind: "spawn_workflow",
        effectId,
        nodes: (Array.isArray(effect.tasks) ? effect.tasks : []).map(raw => {
          const task = asObject(raw)
          const spec = asObject(task.spec)
          return {
            agent_id: String(task.task_id ?? ""),
            task_id: String(task.task_id ?? ""),
            attempt_id: String(task.attempt_id ?? ""),
            launch_token: String(task.launch_token ?? ""),
            node_id: String(task.node_id ?? ""),
            goal: String(spec.goal ?? ""),
            role: String(spec.role ?? "custom"),
            isolation: String(spec.isolation ?? "shared"),
            context_inheritance: String(spec.context_inheritance ?? "none"),
            ...(spec.metadata && typeof spec.metadata === "object"
              ? asObject(spec.metadata)
              : {}),
          }
        }),
        ...(effect.budget ? { budget: asObject(effect.budget) } : {}),
      }
    case "preempt_tasks": {
      const attempts = (Array.isArray(effect.attempts) ? effect.attempts : []).map(raw => {
        const attempt = asObject(raw)
        return {
          task_id: String(attempt.task_id ?? ""),
          attempt_id: String(attempt.attempt_id ?? ""),
        }
      })
      return {
        kind: "preempt_sub_agents",
        effectId,
        attempts,
        agentIds: attempts.map(attempt => attempt.task_id),
        reason: String(effect.reason ?? ""),
      }
    }
    case "persist_memory":
      return {
        kind: "persist_memory",
        effectId,
        memory: asObject(effect.memory),
      }
    case "query_memory":
      return {
        kind: "query_memory",
        effectId,
        query: asObject(effect.query),
        requestedK: Number(effect.requested_k ?? 0),
      }
    case "archive_page_out": {
      const payload = asObject(effect.payload) as {
        content: string
        digest: string
        original_size: string
        preview?: string
      }
      let archived: Message[] = []
      try {
        const decoded = JSON.parse(String(payload.content ?? "")) as unknown
        if (Array.isArray(decoded)) {
          archived = decoded.map(value => kernelMessageToSdk(asObject(value)))
        }
      } catch {
        // Persistence still uses the opaque body and digest. Only optional presentation-side
        // summarization is skipped if the archived message batch cannot be decoded.
      }
      const compressed = (plannedStep.observations ?? [])
        .find(observation => observation.kind === "compressed")
      const pressureAction = compressed ? String(compressed.action ?? "") : ""
      return {
        kind: "archive_page_out",
        effectId,
        handleId: String(effect.handle_id ?? ""),
        payload,
        archived,
        ...(pressureAction ? { action: pressureAction } : {}),
        ...(compressed?.summary ? { summary: String(compressed.summary) } : {}),
        ...(pressureAction
          ? {
              tier: ["context_collapse", "auto_compact"].includes(pressureAction)
                ? "semantic"
                : "durable",
            }
          : {}),
      }
    }
    case "load_payload":
      return {
        kind: "load_payload",
        effectId,
        handleId: String(effect.handle_id ?? ""),
        payloadRef: String(effect.payload_ref ?? ""),
      }
    case "evaluate_milestone": {
      const request = asObject(effect.request)
      return {
        kind: "evaluate_milestone",
        effectId,
        phaseId: String(request.phase_id ?? ""),
        criteria: [],
        requiredEvidence: [],
      }
    }
    default:
      return {
        kind: "unsupported_effect",
        effectId,
        effectKind: String(effect.kind),
      }
  }
}

export class CanonicalKernelRejectedError extends Error {
  readonly fault: Record<string, unknown>

  constructor(faultJson: string) {
    let fault: Record<string, unknown>
    try {
      fault = JSON.parse(faultJson) as Record<string, unknown>
    } catch {
      fault = { code: "invalid_fault", message: faultJson }
    }
    super(`${String(fault.code ?? "kernel_rejected")}: ${String(fault.message ?? "canonical input rejected")}`)
    this.name = "CanonicalKernelRejectedError"
    this.fault = fault
  }
}

/**
 * A record is already authoritative once the journal append returns. A later native commit failure
 * is therefore a rebuild boundary, never an abort boundary.
 */
export class CanonicalKernelRebuildRequiredError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options)
    this.name = "CanonicalKernelRebuildRequiredError"
  }
}

function chainPosition(value: string, label: string): number {
  if (!/^(0|[1-9]\d*)$/.test(value)) {
    throw new RangeError(`${label} must be a canonical decimal integer`)
  }
  const parsed = Number(value)
  if (!Number.isSafeInteger(parsed) || parsed < 0 || parsed >= MAX_CHAIN_POSITION) {
    throw new RangeError(`${label} must be below ${MAX_CHAIN_POSITION}`)
  }
  return parsed
}

function parsePlannedStep(json: string | undefined): CanonicalPlannedStep {
  if (!json) throw new Error("canonical replay is missing plannedStepJson")
  return JSON.parse(json) as CanonicalPlannedStep
}

function parseAdvice(json: string | undefined): Record<string, unknown> | undefined {
  return json ? JSON.parse(json) as Record<string, unknown> : undefined
}

function isCasConflict(error: unknown): boolean {
  return error instanceof JournalCasConflictError ||
    (error as { name?: unknown } | null | undefined)?.name === "JournalCasConflictError"
}

function isCheckpointRequired(preparation: CanonicalPreparation): boolean {
  if (preparation.status !== "rejected") return false
  try {
    const fault = JSON.parse(preparation.faultJson) as { code?: unknown }
    return fault.code === "checkpoint_required"
  } catch {
    return false
  }
}

/**
 * WASM's canonical durable staged-transition host.
 *
 * It owns no scheduler truth. Core prepares opaque record bytes; `KernelJournal` makes those bytes
 * authoritative; only then may core commit and expose the planned effects/terminal to the runner.
 */
export class CanonicalKernelHost {
  constructor(
    readonly kernel: CanonicalKernelInstance,
    readonly journal: KernelJournal,
    readonly operationId: string,
  ) {
    if (!operationId) throw new TypeError("canonical kernel operationId must not be empty")
  }

  async transition(
    input: CanonicalKernelInput,
    options: CanonicalTransitionOptions = {},
  ): Promise<CanonicalTransition> {
    const inputJson = JSON.stringify({
      abi_version: kernelAbiVersion(),
      operation_id: this.operationId,
      input_id: options.inputId ?? `wasm-input-${crypto.randomUUID()}`,
      observed_at_ms: options.observedAtMs ?? String(Date.now()),
      input,
    })
    await this.journal.stageOutboundEnvelope(this.operationId, inputJson)
    try {
      const transition = await this.transitionEnvelope(inputJson, 1, 1)
      await this.journal.clearOutboundEnvelope(this.operationId)
      return transition
    } catch (error) {
      // Append-acked records own the input; rejected inputs will not retry this envelope.
      // Append-before failures leave the staged bytes so wake can drain byte-identical retries.
      if (
        error instanceof CanonicalKernelRebuildRequiredError
        || error instanceof CanonicalKernelRejectedError
      ) {
        await this.journal.clearOutboundEnvelope(this.operationId)
      }
      throw error
    }
  }

  /** Restore the latest installed checkpoint and its authoritative record tail in place. */
  async restore(): Promise<CanonicalRestoreCost> {
    const checkpoint = await this.journal.latestCheckpoint(this.operationId)
    const records = await this.journal.recordsAfter(this.operationId, checkpoint?.covered_head)
    return this.kernel.restore(
      checkpoint ? checkpoint.checkpoint_bytes : undefined,
      records.map(record => record.record_bytes),
    )
  }

  /**
   * Replay a crash-window outbound envelope with identical bytes (adjudication 5e.3).
   * No-op when nothing is staged. Clears the stage after commit/replay/reject.
   */
  async drainOutboundEnvelope(): Promise<CanonicalTransition | undefined> {
    const pending = await this.journal.readOutboundEnvelope(this.operationId)
    if (!pending) return undefined
    try {
      const transition = await this.transitionEnvelope(pending, 1, 1)
      await this.journal.clearOutboundEnvelope(this.operationId)
      return transition
    } catch (error) {
      if (
        error instanceof CanonicalKernelRebuildRequiredError
        || error instanceof CanonicalKernelRejectedError
      ) {
        await this.journal.clearOutboundEnvelope(this.operationId)
      }
      throw error
    }
  }

  /** Execute the full §12.3 install/ack/reclaim boundary. */
  async checkpoint(): Promise<InstalledCheckpoint> {
    const candidate = this.kernel.checkpointCandidate()
    const throughStepSeq = chainPosition(candidate.throughStepSeq, "checkpoint throughStepSeq")
    const previous = await this.journal.latestCheckpoint(this.operationId)
    let installed: InstalledCheckpoint
    try {
      installed = await this.journal.compareAndInstallCheckpoint(
        this.operationId,
        previous?.checkpoint_id,
        candidate.coveredHead,
        {
          checkpoint_id: candidate.ackToken,
          through_step_seq: throughStepSeq,
          state_digest: candidate.stateDigest,
          checkpoint_bytes: candidate.checkpointBytes,
        },
      )
    } catch (error) {
      if (!isCasConflict(error)) throw error
      const winner = await this.journal.latestCheckpoint(this.operationId)
      if (!winner || winner.checkpoint_id !== candidate.ackToken) throw error
      installed = winner
    }

    await this.journal.ackCheckpoint(this.operationId, installed.checkpoint_id)
    this.kernel.ackCheckpoint(candidate.throughStepSeq, candidate.coveredHead)
    await this.journal.pruneAckedPrefix(this.operationId)
    return { ...installed, acknowledged: true }
  }

  private async transitionEnvelope(
    inputJson: string,
    casRetriesLeft: number,
    checkpointRetriesLeft: number,
  ): Promise<CanonicalTransition> {
    const preparation = this.kernel.prepare(inputJson)
    if (preparation.status === "rejected") {
      if (checkpointRetriesLeft > 0 && isCheckpointRequired(preparation)) {
        await this.checkpoint()
        return this.transitionEnvelope(inputJson, casRetriesLeft, checkpointRetriesLeft - 1)
      }
      throw new CanonicalKernelRejectedError(preparation.faultJson)
    }
    if (preparation.status === "replayed") {
      return {
        inputJson,
        stepSeq: chainPosition(preparation.stepSeq, "replayed stepSeq"),
        recordDigest: preparation.recordDigest,
        plannedStep: parsePlannedStep(preparation.plannedStepJson),
        replayed: true,
      }
    }

    const stepSeq = chainPosition(preparation.stepSeq, "prepared stepSeq")
    let appended = false
    try {
      const receipt = await this.journal.compareAndAppend(
        this.operationId,
        preparation.expectedHead,
        {
          step_seq: stepSeq,
          record_digest: preparation.recordDigest,
          record_bytes: preparation.recordBytes,
        },
      )
      appended = true
      const committed = this.kernel.commit(preparation.prepareToken, receipt.record_digest)
      if (
        committed.recordDigest !== preparation.recordDigest ||
        chainPosition(committed.stepSeq, "committed stepSeq") !== stepSeq
      ) {
        throw new Error("canonical commit receipt disagrees with the durably appended record")
      }
      const checkpointAdvice = parseAdvice(committed.checkpointAdviceJson)
      const transition: CanonicalTransition = {
        inputJson,
        stepSeq,
        recordDigest: committed.recordDigest,
        plannedStep: parsePlannedStep(committed.plannedStepJson),
        ...(checkpointAdvice ? { checkpointAdvice } : {}),
        replayed: false,
      }
      if (checkpointAdvice) await this.checkpoint()
      return transition
    } catch (error) {
      if (appended) {
        try {
          await this.restore()
        } catch (restoreError) {
          throw new CanonicalKernelRebuildRequiredError(
            "canonical record is durable, commit failed, and journal rebuild also failed",
            { cause: new AggregateError([error, restoreError]) },
          )
        }
        throw new CanonicalKernelRebuildRequiredError(
          "canonical record is durable but commit could not be published; runtime rebuilt from journal",
          { cause: error },
        )
      }

      try {
        this.kernel.abort(preparation.prepareToken)
      } catch (abortError) {
        throw new AggregateError([error, abortError], "canonical append failed and prepare could not be aborted")
      }
      if (casRetriesLeft > 0 && isCasConflict(error)) {
        await this.restore()
        return this.transitionEnvelope(inputJson, casRetriesLeft - 1, checkpointRetriesLeft)
      }
      throw error
    }
  }
}

export interface CanonicalRunnerRuntimeOptions {
  maxContextTokens: number
  maxTurns?: number
  maxTotalTokens?: number
  maxWallMs?: number
  memoryBindingId?: string
  persistPayload?: (callId: string, content: string, previewBytes: number) => Promise<{
    payloadRef: string
    digest: string
    originalSize: string
    preview: string
  }>
}

function canonicalProviderMessage(raw: Record<string, unknown>): Record<string, unknown> {
  const content = raw.content
  return {
    role: String(raw.role ?? "assistant"),
    content: typeof content === "string" ? content : JSON.stringify(content ?? ""),
    ...((Array.isArray(raw.tool_calls) && raw.tool_calls.length > 0)
      ? {
          tool_calls: raw.tool_calls.map(value => {
            const call = asObject(value)
            return {
              call_id: String(call.call_id ?? call.id ?? ""),
              name: String(call.name ?? ""),
              arguments: canonicalProviderToolArguments(
                String(call.name ?? ""),
                asObject(call.arguments),
              ),
            }
          }),
        }
      : {}),
    ...(raw.token_count !== undefined ? { tokens: Number(raw.token_count) } : {}),
  }
}

function canonicalProviderToolArguments(
  name: string,
  argumentsValue: Record<string, unknown>,
): Record<string, unknown> {
  if (name === "start_workflow") {
    const wrapped = asObject(argumentsValue.spec)
    return canonicalWorkflowSpec(Object.keys(wrapped).length > 0 ? wrapped : argumentsValue)
  }
  if (name === "submit_workflow_nodes") {
    const spec = canonicalWorkflowSpec({
      nodes: Array.isArray(argumentsValue.nodes) ? argumentsValue.nodes : [],
    })
    return { nodes: spec.nodes }
  }
  return argumentsValue
}

function canonicalInitialMessage(raw: Record<string, unknown>): Record<string, unknown> {
  if (Array.isArray(raw.content)) {
    return {
      role: String(raw.role ?? "user"),
      content: encodeCanonicalContentParts(raw.content),
      ...(raw.token_count !== undefined ? { tokens: Number(raw.token_count) } : {}),
    }
  }
  const message = canonicalProviderMessage(raw)
  delete message.tool_calls
  return message
}

function logicalRunSpec(raw: Record<string, unknown> | undefined, goal: string): Record<string, unknown> | undefined {
  if (!raw) return undefined
  const filter = asObject(raw.capability_filter)
  return {
    goal: String(raw.goal ?? goal),
    ...(raw.role ? { role: raw.role } : {}),
    ...(raw.isolation ? { isolation: raw.isolation } : {}),
    ...(raw.context_inheritance ? { context_inheritance: raw.context_inheritance } : {}),
    ...(raw.verification_contract_id ? { verification_contract_id: raw.verification_contract_id } : {}),
    ...((Array.isArray(filter.allowed_kinds) && filter.allowed_kinds.length > 0) ||
      (Array.isArray(filter.allowed_ids) && filter.allowed_ids.length > 0)
      ? {
          capability_filter: {
            ...(Array.isArray(filter.allowed_kinds) && filter.allowed_kinds.length > 0
              ? { allowed_kinds: filter.allowed_kinds }
              : {}),
            ...(Array.isArray(filter.allowed_ids) && filter.allowed_ids.length > 0
              ? { allowed_ids: filter.allowed_ids }
              : {}),
          },
        }
      : {}),
    ...(Object.prototype.hasOwnProperty.call(raw, "exposure_baseline")
      ? { exposure_baseline: raw.exposure_baseline }
      : {}),
    ...(raw.loop_round && typeof raw.loop_round === "object"
      ? {
          loop_round: {
            ...(asObject(raw.loop_round).max_rounds !== undefined
              ? { max_rounds: Number(asObject(raw.loop_round).max_rounds) }
              : {}),
            ...(asObject(raw.loop_round).min_sleep_ms !== undefined
              ? { min_sleep_ms: String(asObject(raw.loop_round).min_sleep_ms) }
              : {}),
            ...(asObject(raw.loop_round).max_sleep_ms !== undefined
              ? { max_sleep_ms: String(asObject(raw.loop_round).max_sleep_ms) }
              : {}),
            ...(asObject(raw.loop_round).default_action !== undefined
              ? { default_action: asObject(raw.loop_round).default_action }
              : {}),
          },
        }
      : {}),
    ...(raw.metadata && typeof raw.metadata === "object" ? { metadata: raw.metadata } : {}),
  }
}

function canonicalWorkflowSpec(raw: Record<string, unknown>): Record<string, unknown> {
  const nodes = Array.isArray(raw.nodes) ? raw.nodes.map(asObject) : []
  const nodeIds = nodes.map((_node, index) => `wf-node${index}`)
  return {
    nodes: nodes.map((node, index) => {
      const unsupported: string[] = []
      if (
        node.kind !== undefined
        || node.reducer !== undefined
        || node.loop !== undefined
        || node.classify !== undefined
        || node.tournament !== undefined
      ) unsupported.push("kind")
      if (node.trust !== undefined && node.trust !== "trusted") unsupported.push("trust")
      const depPolicy = node.dep_policy ?? node.depPolicy
      if (depPolicy !== undefined && depPolicy !== "all_success") unsupported.push("dep_policy")
      if (node.token_budget !== undefined || node.tokenBudget !== undefined) unsupported.push("token_budget")
      if (node.max_turns !== undefined || node.maxTurns !== undefined) unsupported.push("max_turns")
      if (node.max_wall_ms !== undefined || node.maxWallMs !== undefined) unsupported.push("max_wall_ms")
      const inheritance = node.context_inheritance ?? node.contextInheritance
      if (unsupported.length > 0) {
        throw new CanonicalKernelRejectedError(JSON.stringify({
          code: "unsupported_effect",
          message: `workflow node ${index} uses fields absent from canonical WorkflowNode: ${unsupported.join(", ")}`,
        }))
      }
      const taskValue = node.task
      const task = asObject(taskValue)
      const goal = typeof taskValue === "string"
        ? taskValue
        : String(task.goal ?? node.goal ?? "")
      const rawDependsOn = Array.isArray(node.depends_on)
        ? node.depends_on
        : Array.isArray(node.dependsOn) ? node.dependsOn : []
      const dependsOn = rawDependsOn.length > 0
        ? rawDependsOn.map(value => nodeIds[Number(value)] ?? String(value))
        : []
      const modelHint = node.model_hint ?? node.modelHint
      const outputSchema = node.output_schema ?? node.outputSchema
      const runSpec = logicalRunSpec({
        goal,
        ...(node.role ? { role: node.role } : {}),
        ...(node.isolation ? { isolation: node.isolation } : {}),
        ...(inheritance ? { context_inheritance: inheritance } : {}),
        ...((modelHint !== undefined || outputSchema !== undefined)
          ? {
              metadata: {
                ...(modelHint !== undefined ? { model_hint: modelHint } : {}),
                ...(outputSchema !== undefined ? { output_schema: outputSchema } : {}),
              },
            }
          : {}),
      }, goal)
      return {
        node_id: nodeIds[index],
        task: {
          goal,
          ...(Array.isArray(task.criteria) && task.criteria.length > 0 ? { criteria: task.criteria } : {}),
          ...(task.lane ? { lane: task.lane } : {}),
        },
        ...(dependsOn.length > 0 ? { depends_on: dependsOn } : {}),
        ...(runSpec ? { run_spec: runSpec } : {}),
      }
    }),
  }
}

export function sha256(value: string): string {
  return syncSha256(value)
}

/** Small synchronous SHA-256 for Worker/browser hosts (no node:crypto). */
function syncSha256(value: string): string {
  const encoder = new TextEncoder()
  const input = encoder.encode(value)
  const bitLength = input.length * 8
  const length = ((input.length + 9 + 63) >> 6) << 6
  const data = new Uint8Array(length)
  data.set(input)
  data[input.length] = 0x80
  new DataView(data.buffer).setUint32(length - 4, bitLength >>> 0, false)
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19])
  const k = [0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]
  const ror = (x: number, n: number) => (x >>> n) | (x << (32 - n))
  for (let offset = 0; offset < data.length; offset += 64) {
    const w = new Uint32Array(64)
    const view = new DataView(data.buffer, offset, 64)
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(i * 4, false)
    for (let i = 16; i < 64; i++) {
      w[i] = (w[i - 16] + (ror(w[i - 15], 7) ^ ror(w[i - 15], 18) ^ (w[i - 15] >>> 3)) + w[i - 7] + (ror(w[i - 2], 17) ^ ror(w[i - 2], 19) ^ (w[i - 2] >>> 10))) >>> 0
    }
    let [a, b, c, d, e, f, g, hh] = h
    for (let i = 0; i < 64; i++) {
      const t1 = (hh + (ror(e, 6) ^ ror(e, 11) ^ ror(e, 25)) + ((e & f) ^ ((~e) & g)) + k[i] + w[i]) >>> 0
      const t2 = ((ror(a, 2) ^ ror(a, 13) ^ ror(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) >>> 0
      hh = g; g = f; f = e; e = (d + t1) >>> 0; d = c; c = b; b = a; a = (t1 + t2) >>> 0
    }
    for (let i = 0; i < 8; i++) h[i] = (h[i] + [a, b, c, d, e, f, g, hh][i]) >>> 0
  }
  return `sha256:${Array.from(h, word => word.toString(16).padStart(8, "0")).join("")}`
}

export { syncSha256 as sha256Hex }

function providerStopReason(value: unknown): string | undefined {
  if (typeof value !== "string" || value.length === 0) return undefined
  const normalized = value.toLowerCase()
  if (["end_turn", "tool_use", "max_tokens", "stop_sequence", "content_filter"].includes(normalized)) {
    return normalized
  }
  return "other"
}

/**
 * Canonical operation runtime used by the WASM host.
 * Every durable transition below is one of the canonical ABI's five input classes; no legacy
 * envelope or synthesized host transaction reaches core or storage.
 */
export class CanonicalRunnerRuntime {
  private readonly host: CanonicalKernelHost
  private readonly config: Record<string, unknown>
  private readonly initialContext: {
    messages: Array<Record<string, unknown>>
    knowledge: Array<Record<string, unknown>>
    capabilities: Array<Record<string, unknown>>
  } = { messages: [], knowledge: [], capabilities: [] }
  private configured = false
  private started = false
  private turns = 0
  private lastAction: KernelRunnerAction | null = null
  private readonly newMessages: Message[] = []
  private readonly hostObservations: KernelObservationLike[] = []
  private spawnedTasks = 0
  private readonly memoryBindingId: string
  private payloadInlineThreshold = 50 * 1024
  private payloadPreviewBytes = 2 * 1024

  constructor(
    kernel: CanonicalKernelInstance,
    journal: KernelJournal,
    operationId: string,
    private readonly options: CanonicalRunnerRuntimeOptions,
  ) {
    this.host = new CanonicalKernelHost(kernel, journal, operationId)
    this.memoryBindingId = options.memoryBindingId ?? "wasm-memory"
    this.config = {
      execution_policy: {
        max_context_tokens: options.maxContextTokens,
        ...(options.maxTurns !== undefined ? { max_turns: options.maxTurns } : {}),
        ...(options.maxTotalTokens !== undefined ? { max_total_tokens: String(options.maxTotalTokens) } : {}),
        ...(options.maxWallMs !== undefined ? { max_wall_ms: String(options.maxWallMs) } : {}),
      },
      host_effect_support: {
        supported: [
          "call_provider",
          "execute_tools",
          "request_approval",
          "spawn_tasks",
          "preempt_tasks",
          "persist_memory",
          "query_memory",
          "archive_page_out",
          "load_payload",
          "evaluate_milestone",
        ],
      },
      kernel_limits: {
        max_json_depth: 64,
        max_collection_entries: 65_536,
        collection_limits: {
          tool_catalog: 4_096,
          skill_catalog: 4_096,
          knowledge_entries: 65_536,
          initial_messages: 65_536,
          capability_grants: 65_536,
          governance_rules: 65_536,
        },
      },
    }
  }

  get operationId(): string {
    return this.host.operationId
  }

  get journal(): KernelJournal {
    return this.host.journal
  }

  turn(): number {
    return this.turns
  }

  isTerminal(): boolean {
    return ["completed", "cancelled", "failed"].includes(this.host.kernel.lifecycle())
  }

  recoveryContentBytes(): number {
    return Math.max(1_024, this.options.maxContextTokens * 4)
  }

  preservedRefs(): string[] {
    return []
  }

  drainNewMessages(): Message[] {
    return this.newMessages.splice(0)
  }

  drainHostObservations(): KernelObservationLike[] {
    return this.hostObservations.splice(0)
  }

  terminal(): Record<string, unknown> | undefined {
    const terminal = this.host.kernel.terminalJson()
    return terminal ? JSON.parse(terminal) as Record<string, unknown> : undefined
  }

  localSubagentsSpawned(): number {
    return this.spawnedTasks
  }

  async restore(): Promise<void> {
    await this.host.restore()
    this.configured = this.host.kernel.lifecycle() !== "created"
    this.started = !["created", "configured"].includes(this.host.kernel.lifecycle())
    // A crash between stage and append-ack leaves a byte-identical envelope; drain it before
    // the host effect loop so retries never remint observed_at_ms.
    await this.host.drainOutboundEnvelope()
    this.lastAction = this.currentAction()
  }

  resumeAction(): KernelRunnerAction | null {
    this.lastAction = this.currentAction()
    return this.lastAction
  }

  async startAgent(
    taskValue: Record<string, unknown>,
    runSpecValue?: Record<string, unknown>,
  ): Promise<KernelRunnerAction | null> {
    await this.ensureConfigured()
    const goal = String(taskValue.goal ?? "")
    const action = await this.commit({
      kind: "start_operation",
      entry: {
        kind: "agent",
        task: {
          goal,
          ...(Array.isArray(taskValue.criteria) && taskValue.criteria.length > 0
            ? { criteria: taskValue.criteria }
            : {}),
        },
        ...(runSpecValue ? { run_spec: logicalRunSpec(runSpecValue, goal) } : {}),
      },
      initial_context: this.initialContext,
    })
    this.started = true
    return action
  }

  async startWorkflow(specValue: Record<string, unknown>): Promise<KernelRunnerAction | null> {
    await this.ensureConfigured()
    const action = await this.commit({
      kind: "start_operation",
      entry: {
        kind: "workflow",
        spec: canonicalWorkflowSpec(specValue),
      },
      initial_context: this.initialContext,
    })
    this.started = true
    return action
  }

  async applyHostEvent(event: Record<string, unknown>): Promise<KernelRunnerAction | null> {
    if (!this.started && this.applyBootstrapEvent(event)) return null

    let input: CanonicalKernelInput | undefined
    switch (event.kind) {
      case "provider_result": {
        const message = canonicalProviderMessage(asObject(event.message))
        this.newMessages.push({
          role: message.role as Message["role"],
          content: String(message.content ?? ""),
          toolCalls: (Array.isArray(message.tool_calls) ? message.tool_calls : []).map(raw => {
            const call = asObject(raw)
            return {
              id: String(call.call_id ?? ""),
              name: String(call.name ?? ""),
              arguments: JSON.stringify(call.arguments ?? {}),
            }
          }),
        })
        this.turns += 1
        input = {
          kind: "resolve_effect",
          effect_id: String(event.effect_id ?? ""),
          outcome: {
            status: "succeeded",
            result: {
              kind: "provider",
              outcome: {
                kind: "completed",
                message,
                ...(event.observed_input_tokens !== undefined
                  ? { observed_input_tokens: Number(event.observed_input_tokens) }
                  : {}),
                ...(event.observed_output_tokens !== undefined
                  ? { observed_output_tokens: Number(event.observed_output_tokens) }
                  : {}),
                ...(providerStopReason(event.stop_reason)
                  ? { stop_reason: providerStopReason(event.stop_reason) }
                  : {}),
              },
            },
          },
        }
        break
      }
      case "provider_error": {
        const message = String(event.message ?? "")
        const contextOverflow = /context|token.*limit|too long/i.test(message)
        input = contextOverflow
          ? {
              kind: "resolve_effect",
              effect_id: String(event.effect_id ?? ""),
              outcome: {
                status: "succeeded",
                result: { kind: "provider", outcome: { kind: "context_overflow" } },
              },
            }
          : this.failedEffect(event, "transport_exhausted", message, true)
        break
      }
      case "tool_results": {
        const results: Array<Record<string, unknown>> = []
        for (const value of Array.isArray(event.results) ? event.results : []) {
          const result = asObject(value)
          const callId = String(result.call_id ?? "")
          const output = String(result.output ?? "")
          const isError = Boolean(result.is_error)
          const disposition = result.is_fatal ? "fatal" : "recoverable"
          const bytes = utf8ByteLength(output)
          if (bytes > this.payloadInlineThreshold && this.options.persistPayload) {
            const persisted = await this.options.persistPayload(
              callId,
              output,
              this.payloadPreviewBytes,
            )
            results.push({
              kind: "external",
              call_id: callId,
              payload_ref: persisted.payloadRef,
              digest: persisted.digest,
              original_size: persisted.originalSize,
              preview: persisted.preview,
              ...(isError ? { is_error: true } : {}),
              disposition,
            })
          } else {
            results.push({
              kind: "inline",
              call_id: callId,
              result: {
                output,
                ...(isError ? { is_error: true } : {}),
                disposition,
                ...(result.token_count !== null && result.token_count !== undefined
                  ? { tokens: Number(result.token_count) }
                  : {}),
              },
            })
          }
          this.newMessages.push({ role: "tool", content: output, toolCalls: [] })
        }
        input = this.succeededEffect(event, { kind: "tools", results })
        break
      }
      case "approval_result":
        input = this.succeededEffect(event, {
          kind: "approval",
          approved_call_ids: Array.isArray(event.approved_calls) ? event.approved_calls : [],
          denied_call_ids: Array.isArray(event.denied_calls) ? event.denied_calls : [],
        })
        break
      case "workflow_spawn_result": {
        const spawn = this.lastAction?.kind === "spawn_workflow" ? this.lastAction : undefined
        this.spawnedTasks += spawn?.nodes.length ?? 0
        input = this.succeededEffect(event, {
          kind: "tasks_spawned",
          attempts: (spawn?.nodes ?? []).map(node => ({
            task_id: String(node.task_id ?? node.agent_id ?? ""),
            attempt_id: String(node.attempt_id ?? ""),
            outcome: { status: "started" },
          })),
        })
        break
      }
      case "preempt_result": {
        const preempt = this.lastAction?.kind === "preempt_sub_agents" ? this.lastAction : undefined
        input = this.succeededEffect(event, {
          kind: "tasks_preempted",
          attempts: (preempt?.attempts ?? []).map(attempt => ({
            ...attempt,
            outcome: { status: "preempted" },
          })),
        })
        break
      }
      case "sub_agent_completed": {
        const raw = asObject(event.result)
        const result = asObject(raw.result)
        const submittedNodes = Array.isArray(raw.submitted_nodes)
          ? raw.submitted_nodes.map(asObject)
          : []
        const taskId = String(raw.agent_id ?? "")
        const pending = this.pendingEffects().find(effect => {
          const kind = asObject(effect.effect)
          return kind.kind === "spawn_tasks" &&
            (Array.isArray(kind.tasks) ? kind.tasks : []).some(task => asObject(task).task_id === taskId)
        })
        const launch = pending
          ? (Array.isArray(asObject(pending.effect).tasks) ? asObject(pending.effect).tasks as unknown[] : [])
              .map(asObject).find(task => task.task_id === taskId)
          : undefined
        input = {
          kind: "deliver_external_event",
          event: {
            kind: "child_completed",
            task_id: taskId,
            attempt_id: String(launch?.attempt_id ?? `${taskId}:attempt:1`),
            result: {
              status: result.termination === "completed" ? "completed" : "failed",
              ...(asObject(result.final_message).content
                ? { output: String(asObject(result.final_message).content) }
                : {}),
              ...(!["completed", "max_turns", "token_budget"].includes(String(result.termination))
                ? { error: String(result.termination ?? "failed") }
                : {}),
              usage: {
                input_tokens: "0",
                output_tokens: String(result.total_tokens_used ?? 0),
                turns: Number(result.turns_used ?? 0),
              },
            },
            ...(submittedNodes.length > 0
              ? {
                  parent_requests: [{
                    kind: "append_workflow_nodes",
                    nodes: asObject(canonicalWorkflowSpec({ nodes: submittedNodes })).nodes,
                  }],
                }
              : {}),
          },
        }
        break
      }
      case "memory_persist_result":
        input = event.error
          ? this.failedEffect(event, "storage_unavailable", String(event.error), true)
          : this.succeededEffect(event, {
              kind: "memory_persisted",
              receipt: {
                binding_id: this.memoryBindingId,
                record_ref: String(event.record_ref ?? `memory:${crypto.randomUUID()}`),
                digest: String(event.digest ?? sha256(String(event.record_ref ?? event.effect_id ?? ""))),
              },
            })
        break
      case "memory_query_result":
        input = event.error
          ? this.failedEffect(event, "storage_unavailable", String(event.error), true)
          : this.succeededEffect(event, {
              kind: "memory_queried",
              recalls: (Array.isArray(event.hits) ? event.hits : []).map(value => {
                const hit = asObject(value)
                const record = asObject(hit.record)
                return {
                  record_ref: String(record.record_id ?? `memory:${crypto.randomUUID()}`),
                  name: String(record.name ?? ""),
                  kind: String(record.kind ?? "reference"),
                  content: String(record.content ?? ""),
                  ...(typeof hit.score === "number" ? { score: hit.score } : {}),
                }
              }),
            })
        break
      case "page_out_archive_result":
        input = event.error
          ? this.failedEffect(event, "storage_unavailable", String(event.error), true)
          : this.pageOutResolution(event)
        break
      case "milestone_result": {
        const result = asObject(event.result)
        input = this.succeededEffect(event, {
          kind: "milestone_evaluated",
          result: {
            phase_id: String(result.phase_id ?? ""),
            passed: Boolean(result.passed),
            ...(!result.passed && result.reason ? { notes: String(result.reason) } : {}),
          },
        })
        break
      }
      case "payload_loaded":
        input = this.succeededEffect(event, {
          kind: "payload_loaded",
          handle_id: String(event.handle_id ?? ""),
          payload: event.payload,
        })
        break
      case "payload_load_failed":
        input = this.failedEffect(event, "storage_unavailable", String(event.error ?? "payload load failed"), true)
        break
      case "cancel_operation":
        input = {
          kind: "host_control",
          command: {
            kind: "cancel",
            reason: event.reason ?? "user",
            pending_call_ids: Array.isArray(event.pending_call_ids) ? event.pending_call_ids : [],
          },
        }
        break
      case "deliver_signal":
        input = { kind: "deliver_external_event", event: this.canonicalSignal(event) }
        break
      case "update_task":
        input = { kind: "host_control", command: { kind: "update_task", update: event.update } }
        break
      case "add_knowledge_message":
        input = {
          kind: "host_control",
          command: {
            kind: "seed_knowledge",
            entries: [{
              content: String(event.content ?? ""),
              ...(event.key ? { key: event.key } : {}),
              ...(event.tokens !== undefined ? { tokens: Number(event.tokens) } : {}),
              ...(event.pinned ? { pinned: true } : {}),
            }],
          },
        }
        break
      case "remove_knowledge":
        input = {
          kind: "host_control",
          command: {
            kind: "apply_knowledge_mutation",
            mutation: { remove: [String(event.key ?? "")] },
          },
        }
        break
      case "skill_deactivated":
        input = {
          kind: "host_control",
          command: {
            kind: "apply_skill_activation",
            deactivate: [String(event.name ?? "")],
          },
        }
        break
      case "unsupported_effect":
        input = canonicalUnsupportedEffectResolution(
          String(event.effect_id ?? ""),
          String(event.effect_kind ?? ""),
        )
        break
      case "capability_command":
        input = { kind: "host_control", command: this.canonicalCapabilityCommand(asObject(event.command)) }
        break
      case "add_history_message":
        throw new Error("running ABI v3 operations accept history only through effects or external events")
      default:
        throw new Error(`WASM host fact has no canonical ABI input: ${String(event.kind)}`)
    }
    return this.commit(input)
  }

  private async ensureConfigured(): Promise<void> {
    if (this.configured) return
    await this.commit({ kind: "configure_operation", config: this.config })
    this.configured = true
  }

  private async commit(input: CanonicalKernelInput): Promise<KernelRunnerAction | null> {
    let nextInput = input
    for (;;) {
      const transition = await this.host.transition(nextInput)
      if (!transition.replayed) {
        for (const raw of transition.plannedStep.observations ?? []) {
          const kind = String(raw.kind ?? "")
          if (!kind) throw new Error("canonical observation is missing kind")
          this.hostObservations.push({ ...raw, kind })
        }
        if (transition.checkpointAdvice) {
          this.hostObservations.push({
            kind: "checkpoint_advised",
            ...transition.checkpointAdvice,
          })
        }
      }
      this.lastAction = canonicalActionFromPlannedStep(transition.plannedStep)
      if (this.lastAction?.kind !== "unsupported_effect") return this.lastAction
      nextInput = canonicalUnsupportedEffectResolution(
        this.lastAction.effectId,
        this.lastAction.effectKind,
      )
    }
  }

  private currentAction(): KernelRunnerAction | null {
    const terminal = this.host.kernel.terminalJson()
    if (terminal) {
      return canonicalActionFromPlannedStep({
        disposition: { kind: "terminal", terminal: JSON.parse(terminal) as Record<string, unknown> },
      })
    }
    const effects = this.pendingEffects()
    return canonicalActionFromPlannedStep({
      disposition: { kind: "effects", effects },
    })
  }

  private pendingEffects(): Array<Record<string, unknown>> {
    return JSON.parse(this.host.kernel.pendingEffectsJson()) as Array<Record<string, unknown>>
  }

  private succeededEffect(
    event: Record<string, unknown>,
    result: Record<string, unknown>,
  ): CanonicalKernelInput {
    return {
      kind: "resolve_effect",
      effect_id: String(event.effect_id ?? ""),
      outcome: { status: "succeeded", result },
    }
  }

  private failedEffect(
    event: Record<string, unknown>,
    kind: string,
    message: string,
    retryable?: boolean,
  ): CanonicalKernelInput {
    return {
      kind: "resolve_effect",
      effect_id: String(event.effect_id ?? ""),
      outcome: {
        status: "failed",
        failure: {
          kind,
          message,
          ...(retryable !== undefined ? { retryable } : {}),
        },
      },
    }
  }

  private pageOutResolution(event: Record<string, unknown>): CanonicalKernelInput {
    const pending = this.pendingEffects().find(effect => effect.effect_id === event.effect_id)
    const effect = asObject(pending?.effect)
    const payload = asObject(effect.payload)
    const content = String(payload.content ?? "")
    return this.succeededEffect(event, {
      kind: "page_out_archived",
      receipt: {
        handle_id: String(effect.handle_id ?? ""),
        payload_ref: String(event.payload_ref ?? `payload:${crypto.randomUUID()}`),
        digest: String(payload.digest ?? sha256(content)),
        original_size: String(payload.original_size ?? utf8ByteLength(content)),
      },
    })
  }

  private canonicalSignal(event: Record<string, unknown>): Record<string, unknown> {
    const signal = asObject(event.signal)
    const deliveryId = String(event.delivery_id ?? crypto.randomUUID())
    const payload = deliveryId.startsWith("injected-") && typeof signal.summary === "string"
      ? signal.summary
      : signal.payload ?? {}
    return {
      kind: "deliver_signal",
      delivery_id: deliveryId,
      attempt: Number(event.attempt ?? 1),
      signal: {
        signal_id: String(signal.signal_id ?? signal.id ?? crypto.randomUUID()),
        ...(signal.source ? { source: signal.source } : {}),
        target: signal.recipient
          ? { kind: "task", task_id: String(signal.recipient) }
          : { kind: "operation" },
        ...(signal.urgency ? { urgency: signal.urgency } : {}),
        payload,
        ...(signal.timestamp_ms !== undefined ? { source_timestamp_ms: String(signal.timestamp_ms) } : {}),
        ...(signal.dedupe_key ? { dedupe_key: signal.dedupe_key } : {}),
      },
    }
  }

  private canonicalCapabilityCommand(command: Record<string, unknown>): Record<string, unknown> {
    const capability = asObject(command.capability)
    if (command.action === "mount") {
      return {
        kind: "apply_capability_patch",
        patch: {
          mount: [{
            kind: String(capability.kind ?? "tool"),
            id: String(capability.id ?? ""),
            ...(capability.description ? { description: capability.description } : {}),
          }],
        },
      }
    }
    return {
      kind: "apply_capability_patch",
      patch: {
        unmount: [{
          kind: String(command.kind ?? "tool"),
          id: String(command.id ?? ""),
        }],
      },
    }
  }

  private applyBootstrapEvent(event: Record<string, unknown>): boolean {
    switch (event.kind) {
      case "set_tokenizer":
        return true
      case "set_plan_tool_enabled":
        this.featurePolicy().plan_tool_enabled = Boolean(event.enabled)
        return true
      case "set_tools":
        this.config.tool_catalog = Array.isArray(event.tools) ? event.tools.map(asObject) : []
        return true
      case "add_system_message":
        this.initialContext.messages.push({
          role: "system",
          content: String(event.content ?? ""),
          ...(event.tokens !== undefined ? { tokens: Number(event.tokens) } : {}),
        })
        return true
      case "add_knowledge_message":
        this.initialContext.knowledge.push({
          content: String(event.content ?? ""),
          ...(event.key ? { key: event.key } : {}),
          ...(event.tokens !== undefined ? { tokens: Number(event.tokens) } : {}),
          ...(event.pinned ? { pinned: true } : {}),
        })
        return true
      case "set_available_skills":
        this.config.skill_catalog = event.skills ?? []
        return true
      case "set_stable_core_tools":
        this.featurePolicy().stable_core_tool_ids = event.tool_ids ?? []
        return true
      case "set_memory_enabled":
        this.featurePolicy().memory_enabled = Boolean(event.enabled)
        if (event.enabled) {
          this.config.memory_access = {
            binding_id: this.memoryBindingId,
            capabilities: { read: true, write: true },
          }
        }
        return true
      case "set_knowledge_enabled":
        this.featurePolicy().knowledge_enabled = Boolean(event.enabled)
        return true
      case "set_memory_policy":
        this.config.memory_policy = {
          ...(event.stale_warning_days !== undefined ? { stale_warning_days: event.stale_warning_days } : {}),
          ...(event.retrieval_top_k !== undefined ? { retrieval_top_k: event.retrieval_top_k } : {}),
          ...(event.validation_enabled !== undefined ? { validation_enabled: event.validation_enabled } : {}),
          ...(event.max_content_bytes !== undefined ? { max_content_bytes: event.max_content_bytes } : {}),
          ...(event.max_name_length !== undefined ? { max_name_length: event.max_name_length } : {}),
          ...(event.promotion_recall_threshold !== undefined
            ? { promotion_recall_threshold: String(event.promotion_recall_threshold) }
            : {}),
        }
        return true
      case "load_milestone_contract": {
        const contract = asObject(event.contract)
        this.config.verification_contracts = [{
          contract_id: "node-default",
          phases: (Array.isArray(contract.phases) ? contract.phases : []).map(value => {
            const phase = asObject(value)
            return {
              phase_id: String(phase.id ?? ""),
              unlocks: (Array.isArray(phase.unlocks) ? phase.unlocks : []).map(unlock => {
                const item = asObject(unlock)
                return typeof unlock === "string" ? unlock : String(item.id ?? "")
              }).filter(Boolean),
            }
          }),
        }]
        return true
      }
      case "preload_history":
        this.initialContext.messages.push(
          ...(Array.isArray(event.messages) ? event.messages : []).map(value =>
            canonicalInitialMessage(asObject(value))),
        )
        return true
      case "add_history_message":
        this.initialContext.messages.push(canonicalInitialMessage(asObject(event.message)))
        return true
      case "configure_run":
        this.mergeHostConfig(asObject(event.config))
        return true
      default:
        return false
    }
  }

  private featurePolicy(): Record<string, unknown> {
    const current = asObject(this.config.feature_policy)
    this.config.feature_policy = current
    return current
  }

  private executionPolicy(): Record<string, unknown> {
    return asObject(this.config.execution_policy)
  }

  private mergeHostConfig(config: Record<string, unknown>): void {
    if (config.governance) {
      const governance = asObject(config.governance)
      this.config.governance_policy = {
        ...governance,
        rate_limits: (Array.isArray(governance.rate_limits) ? governance.rate_limits : []).map(value => {
          const rule = asObject(value)
          return { ...rule, window_ms: String(rule.window_ms ?? 0) }
        }),
      }
    }
    if (config.context_policy) this.config.context_policy = config.context_policy
    if (config.signal_policy) {
      const signal = asObject(config.signal_policy)
      const { version: _version, ...rest } = signal
      this.config.signal_policy = {
        ...rest,
        ...(rest.ttl_ms !== undefined ? { ttl_ms: String(rest.ttl_ms) } : {}),
      }
    }
    if (config.scheduler_policy) {
      const scheduler = asObject(config.scheduler_policy)
      const { version: _version, ...rest } = scheduler
      this.config.scheduler_policy = rest
    }
    if (config.resource_quota) {
      const quota = asObject(config.resource_quota)
      const window = Array.isArray(quota.memory_writes_per_window)
        ? quota.memory_writes_per_window
        : undefined
      this.config.resource_quota = {
        ...quota,
        ...(window
          ? {
              memory_writes_per_window: {
                max_events: Number(window[0] ?? 0),
                window_ms: String(window[1] ?? 0),
              },
            }
          : {}),
      }
    }
    if (config.budget_grant) {
      const grant = asObject(config.budget_grant)
      this.config.budget_grant = {
        ...grant,
        ...(grant.tokens !== undefined ? { tokens: String(grant.tokens) } : {}),
      }
    }
    if (config.prompt_budget) {
      const context = asObject(this.config.context_policy)
      context.prompt_budget = config.prompt_budget
      this.config.context_policy = context
    }
    const execution = this.executionPolicy()
    if (config.repeat_fuse) execution.repeat_fuse = config.repeat_fuse
    if (config.criteria_gate !== undefined) execution.criteria_gate_enabled = config.criteria_gate
    if (config.entropy_watch) {
      const entropy = asObject(config.entropy_watch)
      execution.entropy_watch = {
        ...entropy,
        ...(typeof entropy.threshold === "number"
          ? { threshold_ppm: Math.round(entropy.threshold * 1_000_000) }
          : {}),
        ...(typeof entropy.hysteresis === "number"
          ? { hysteresis_ppm: Math.round(entropy.hysteresis * 1_000_000) }
          : {}),
      }
      delete asObject(execution.entropy_watch).threshold
      delete asObject(execution.entropy_watch).hysteresis
    }
    if (config.tool_dispatch_gate !== undefined) {
      this.featurePolicy().tool_dispatch_gate = config.tool_dispatch_gate
    }
    if (config.knowledge_budget_ratio !== undefined) {
      const context = asObject(this.config.context_policy)
      context.knowledge_budget_ppm = Math.round(Number(config.knowledge_budget_ratio) * 1_000_000)
      this.config.context_policy = context
    }
    if (config.reliability) {
      const reliability = asObject(config.reliability)
      this.config.recovery_policy = {
        ...(reliability.provider_recovery_attempts !== undefined
          ? { provider_recovery_attempts: reliability.provider_recovery_attempts }
          : {}),
        ...(reliability.output_recovery_attempts !== undefined
          ? { output_recovery_attempts: reliability.output_recovery_attempts }
          : {}),
      }
      if (reliability.max_input_bytes !== undefined) {
        this.config.kernel_limits = {
          ...asObject(this.config.kernel_limits),
          max_input_bytes: reliability.max_input_bytes,
        }
      }
    }
  }
}

export async function canonicalKernelApply(
  runtime: CanonicalRunnerRuntime,
  pending: KernelObservationLike[],
  event: Record<string, unknown>,
): Promise<KernelObservationLike[]> {
  await runtime.applyHostEvent(event)
  const observations = runtime.drainHostObservations()
  pending.push(...observations)
  return observations
}

export async function canonicalKernelMaybeAction(
  runtime: CanonicalRunnerRuntime,
  pending: KernelObservationLike[],
  event: Record<string, unknown>,
): Promise<KernelRunnerAction | null> {
  const action = await runtime.applyHostEvent(event)
  pending.push(...runtime.drainHostObservations())
  return action
}

export async function canonicalKernelAction(
  runtime: CanonicalRunnerRuntime,
  pending: KernelObservationLike[],
  event: Record<string, unknown>,
): Promise<KernelRunnerAction> {
  const action = await canonicalKernelMaybeAction(runtime, pending, event)
  if (!action) throw new Error("canonical kernel transition must return one host action")
  return action
}

export async function canonicalStartAgent(
  runtime: CanonicalRunnerRuntime,
  pending: KernelObservationLike[],
  task: Record<string, unknown>,
  runSpec?: Record<string, unknown>,
): Promise<KernelRunnerAction> {
  const action = await runtime.startAgent(task, runSpec)
  pending.push(...runtime.drainHostObservations())
  if (!action) throw new Error("canonical agent root must return one host action")
  return action
}

export async function canonicalStartWorkflow(
  runtime: CanonicalRunnerRuntime,
  pending: KernelObservationLike[],
  spec: Record<string, unknown>,
): Promise<KernelRunnerAction | null> {
  const action = await runtime.startWorkflow(spec)
  pending.push(...runtime.drainHostObservations())
  return action
}

type KernelObservationLike = KernelObservation
