import type {
  EntropySample,
  Message,
  RenderedContext,
  TaskUpdate,
  ToolCall,
  ToolResult,
  ToolSchema,
} from "../types.js"
import type { SkillMetadata } from "../skills/loader.js"
import type { RollbackReason } from "./session-log.js"
import type { SessionLog } from "./session-log.js"
import {
  KernelLogConflictError,
  KernelLogIntegrityError,
  createKernelOperationGenesis,
  createKernelTransaction,
  kernelRecordDigest,
  type KernelOperationGenesis,
  type KernelTransaction,
} from "./kernel-transaction-log.js"
// Runtime import; `kernel-rebuild.ts` only imports *types* from this module, so there is no cycle.
import { rebuildKernelRuntime } from "./kernel-rebuild.js"

export const KERNEL_ABI_VERSION = 2
const CANONICAL_CONTENT_PARTS_PREFIX = "[[deepstrike-content-parts:v1]]"

export function encodeCanonicalContentParts(parts: unknown[]): string {
  return `${CANONICAL_CONTENT_PARTS_PREFIX}${Buffer.from(JSON.stringify(parts)).toString("base64url")}`
}

function decodeCanonicalContentParts(content: string): Array<Record<string, unknown>> | undefined {
  if (!content.startsWith(CANONICAL_CONTENT_PARTS_PREFIX)) return undefined
  try {
    const decoded = JSON.parse(
      Buffer.from(content.slice(CANONICAL_CONTENT_PARTS_PREFIX.length), "base64url").toString("utf8"),
    ) as unknown
    return Array.isArray(decoded)
      ? decoded.filter((part): part is Record<string, unknown> =>
          Boolean(part) && typeof part === "object")
      : undefined
  } catch {
    return undefined
  }
}

export interface KernelRuntimeHandle {
  step(inputJson: string): string
  prepareStep(inputJson: string): string
  commitPrepared(prepareToken: string): string
  abortPrepared(prepareToken: string): void
  snapshot(): string
  restore(snapshotJson: string): void
  diagnostics(): string
  isTerminal(): boolean
  turn(): number
  recoveryContentBytes(): number
  render(): RenderedContext
  drainNewMessages(): Message[]
  preservedRefs(): string[]
}

export type KernelPreparationStatus = "prepared" | "replayed" | "rejected"

export interface KernelPreparedStep {
  status: KernelPreparationStatus
  base_generation: number
  prepare_token?: string
  input: Record<string, unknown>
  step: KernelStepJson
}

export interface KernelDiagnostics {
  lifecycle: string
  next_step_seq: number
  accepted_input_count: number
  accepted_input_bytes: number
  snapshot_input_limit: number
  snapshot_journal_bytes_limit: number
  max_input_bytes: number
  snapshot_overflowed: boolean
  recorded_event_count: number
  completed_effect_count: number
  pending_effect_count: number
}

export function readKernelDiagnostics(runtime: KernelRuntimeHandle): KernelDiagnostics {
  return JSON.parse(runtime.diagnostics()) as KernelDiagnostics
}

export interface KernelSnapshot {
  snapshot_version: 2
  abi_version: 2
  initial_policy: {
    max_tokens: number
    max_turns: number
    max_total_tokens: string
    max_wall_ms?: string
  }
  lifecycle: string
  operation_id?: string
  next_step_seq: number
  snapshot_input_limit: number
  max_input_bytes: number
  snapshot_journal_bytes_limit: number
  accepted_input_bytes: number
  accepted_inputs: Array<{ event_id: string; [key: string]: unknown }>
  last_step?: Record<string, unknown>
}

export function snapshotKernelRuntime(runtime: KernelRuntimeHandle): KernelSnapshot {
  return JSON.parse(runtime.snapshot()) as KernelSnapshot
}

export function restoreKernelRuntime(runtime: KernelRuntimeHandle, snapshot: KernelSnapshot): void {
  runtime.restore(JSON.stringify(snapshot))
  const operationId = snapshot.operation_id
  if (!operationId) {
    kernelWireStates.delete(runtime)
    return
  }
  const nextEventSequence = snapshot.accepted_inputs.reduce((next, input) => {
    const match = input.event_id.match(/-event-(\d+)$/)
    return match ? Math.max(next, Number(match[1]) + 1) : next
  }, 1)
  kernelWireStates.set(runtime, { operationId, nextEventSequence })
}

export interface PaceDecision {
  action: "continue" | "sleep" | "stop"
  delayMs?: number
  reason: string
  /** Set when the kernel trap coerced the model's proposal (clamped delay / forced stop). */
  coercedFrom?: string
}

export interface KernelLoopResult {
  termination: string
  turnsUsed: number
  totalTokensUsed: number
  finalMessage?: Message
  /** ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs). */
  paceDecision?: PaceDecision
}

export type MilestoneVerifierKind =
  | { kind: "machine_check" }
  | { kind: "harness_eval" }
  | { kind: "llm_judge" }
  | { kind: "human_approval" }
  | { kind: "external_command"; cmd: string }

export type KernelRunnerAction =
  | { kind: "call_provider"; effectId: string; context: RenderedContext; tools: ToolSchema[] }
  | { kind: "execute_tool"; effectId: string; calls: ToolCall[] }
  | {
      kind: "request_approval"
      effectId: string
      requests: Array<{ callId: string; tool: string; arguments: string; reason: string }>
    }
  | {
      kind: "spawn_workflow"
      effectId: string
      nodes: Array<Record<string, unknown>>
      budget?: Record<string, unknown>
    }
  | {
      kind: "preempt_sub_agents"
      effectId: string
      agentIds: string[]
      attempts?: Array<{ task_id: string; attempt_id: string }>
      reason: string
    }
  | { kind: "persist_memory"; effectId: string; memory: Record<string, unknown> }
  | { kind: "query_memory"; effectId: string; query: Record<string, unknown>; requestedK: number }
  | {
      kind: "spool_large_result"
      effectId: string
      callId: string
      tool: string
      output: string
      originalSize: number
      previewSize: number
    }
  | {
      kind: "archive_page_out"
      effectId: string
      turn?: number
      action?: string
      summary?: string
      archived?: Message[]
      tier?: string
      handleId?: string
      payload?: {
        content: string
        digest: string
        original_size: string
        preview?: string
      }
    }
  | { kind: "load_payload"; effectId: string; handleId: string; payloadRef: string }
  | {
      kind: "evaluate_milestone"
      effectId: string
      phaseId: string
      criteria: string[]
      verifier?: MilestoneVerifierKind
      requiredEvidence: string[]
    }
  | { kind: "done"; effectId: string; result: KernelLoopResult }

export interface KernelObservation {
  kind: string
  /** control_request_rejected: stable control-plane operation name and optional subject id. */
  operation?: string
  subject?: string
  action?: string
  rho_after?: number
  sprint?: number
  summary?: string
  archived_count?: number
  turn?: number
  checkpoint_history_len?: number
  history_len?: number
  added?: string[]
  removed?: string[]
  change_kind?: string
  capability_id?: string
  version?: string
  mounted_by?: string
  mount_reason?: string
  phase_id?: string
  capabilities_unlocked?: string[]
  evidence?: string[]
  // K1 `knowledge_swept`: keyed entries dropped by a boundary sweep of the knowledge partition.
  removed_keys?: string[]
  tokens_freed?: number
  // `reason` is a RollbackReason for rollback observations, but a plain string
  // for `tool_gated` (governance AskUser) — consumers narrow by `kind`.
  reason?: RollbackReason | string
  agent_id?: string
  parent_task_id?: string
  parent_session_id?: string
  role?: string
  isolation?: string
  context_inheritance?: string
  permitted_capability_ids?: string[]
  // tool_gated (governance AskUser): the call needing user approval.
  call_id?: string
  tool?: string
  // signal_delivery_disposed: the correlated routing decision.
  operation_id?: string
  delivery_id?: string
  attempt?: number
  signal_id?: string
  disposition?: string
  queue_depth?: number
  // Phase 2: budget_exceeded observation — which budget axis fired.
  budget?: string
  reservation_id?: string
  tokens?: number
  subagents?: number
  rounds?: number
  // Phase 2: suspended observation — loop suspended awaiting external resolution.
  pending_calls?: string[]
  pending_call_ids?: string[]
  // Phase 2: resumed observation — loop resumed with approved/denied calls.
  approved?: string[]
  denied?: string[]
  tier?: string
  message_count?: number
  archive_ref?: string
  spool_ref?: string
  original_size?: number
  preview_size?: number
  // Phase 7 / M3: Memory observations
  record_id?: string
  scope?: { tenant_id: string; namespace: string }
  name?: string
  memory_kind?: string
  size_bytes?: number
  query?: string
  requested_k?: number
  requires_async_response?: boolean
  // M3 memory_recalled: journaled recall lifecycle mirrored into the durable store.
  recalls?: Array<{ record_id: string; recall_count: number; last_recalled_at: number }>
  // M4 promotion_suggested: a recalled record crossed the promotion threshold (advisory).
  recall_count?: number
  /** memory_validation_failed (Phase 7). */
  error?: string
  // W0-ABI: workflow lifecycle observations.
  /** workflow_batch_spawned: per-node spawn descriptors (agent_id + goal + role/isolation). */
  nodes?: Array<{
    agent_id: string
    goal: string
    role: string
    isolation: string
    context_inheritance: string
    model_hint?: string
    trust?: string
  }>
  /** workflow_completed. */
  node_outcomes?: import("../types/agent.js").KernelWorkflowNodeOutcome[]
  /** nodes_rejected. */
  node_index?: number
  // entropy_sample / entropy_alert: kernel session-entropy measurement + opt-in watch trip.
  score?: number
  score_version?: number
  rho?: number
  repeat_pressure?: number
  failure_rate?: number
  rollbacks_in_window?: number
  window_turns?: number
  threshold?: number
}

export interface KernelStepJson {
  version: number
  operation_id: string
  input_event_id: string
  step_seq: number
  actions: Array<Record<string, unknown>>
  observations: KernelObservation[]
  faults?: Array<{ code?: string; message?: string; effect_id?: string }>
}

interface KernelWireState {
  operationId: string
  nextEventSequence: number
}

const kernelWireStates = new WeakMap<object, KernelWireState>()

function tryParseJson(s: string): unknown {
  try {
    return JSON.parse(s)
  } catch {
    return null
  }
}

export function toolSchemaToKernel(schema: ToolSchema): Record<string, unknown> {
  return {
    name: schema.name,
    description: schema.description,
    parameters: tryParseJson(schema.parameters) ?? {},
  }
}

export function skillMetadataToKernel(skill: SkillMetadata): Record<string, unknown> {
  const out: Record<string, unknown> = {
    name: skill.name,
    description: skill.description,
    estimated_tokens: skill.estimatedTokens ?? 0,
  }
  if (skill.whenToUse) out.when_to_use = skill.whenToUse
  if (skill.effort !== undefined) out.effort = skill.effort
  // P1-B: forward declared tool ids (additive; omitted when empty so existing skills' wire is unchanged).
  if (skill.allowedTools?.length) out.allowed_tools = skill.allowedTools
  return out
}

export function messageToKernelMessage(message: Message): Record<string, unknown> {
  const out: Record<string, unknown> = {
    role: message.role,
    tool_calls: (message.toolCalls ?? []).map(tc => ({
      id: tc.id,
      name: tc.name,
      arguments: tryParseJson(tc.arguments) ?? {},
    })),
  }
  if (message.tokenCount !== undefined) {
    out.token_count = message.tokenCount
  }
  if (message.contentParts && message.contentParts.length > 0) {
    out.content = message.contentParts.map(part => {
      if (part.type === "text") return { type: "text", text: part.text }
      if (part.type === "tool_result") {
        return {
          type: "tool_result",
          call_id: part.callId,
          output: part.output,
          is_error: part.isError,
        }
      }
      if (part.type === "image") {
        return {
          type: "image",
          url: part.url,
          data: part.data,
          media_type: part.mediaType,
          detail: part.detail,
        }
      }
      if (part.type === "audio") {
        return { type: "audio", data: part.data, media_type: part.mediaType }
      }
      return { type: "text", text: message.content }
    })
  } else {
    out.content = message.content
  }
  return out
}

export function toolResultToKernel(result: ToolResult): Record<string, unknown> {
  const out: Record<string, unknown> = {
    call_id: result.callId,
    output: result.output,
    is_error: result.isError,
    is_fatal: result.isFatal ?? false,
    token_count: result.tokenCount ?? null,
  }
  if (result.errorKind !== undefined) {
    out.error_kind = result.errorKind
  }
  return out
}

export function taskUpdateToKernel(update: TaskUpdate): Record<string, unknown> {
  return {
    plan: update.plan,
    current_step: update.currentStep,
    progress: update.progress,
    scratchpad: update.scratchpad,
    blocked_on: update.blockedOn,
    preserved_refs: update.preservedRefs,
  }
}

export function capabilityTool(schema: ToolSchema): Record<string, unknown> {
  return {
    id: schema.name,
    kind: "tool",
    description: schema.description,
    tool_schema: toolSchemaToKernel(schema),
  }
}

export function capabilitySkill(skill: SkillMetadata): Record<string, unknown> {
  return {
    id: skill.name,
    kind: "skill",
    description: skill.description,
    skill: skillMetadataToKernel(skill),
  }
}

export function capabilityMarker(kind: string, id: string, description: string): Record<string, unknown> {
  return { id, kind, description }
}

export function capabilityCommandMount(
  capability: Record<string, unknown>,
  mountedBy = "sdk:runtime",
  mountReason = "dynamic_register",
): Record<string, unknown> {
  return {
    kind: "capability_command",
    command: {
      action: "mount",
      capability,
      mounted_by: mountedBy,
      mount_reason: mountReason,
    },
  }
}

export function capabilityCommandUnmount(capabilityKind: string, id: string): Record<string, unknown> {
  return {
    kind: "capability_command",
    command: { action: "unmount", kind: capabilityKind, id },
  }
}

function parseStep(raw: string): KernelStepJson {
  return JSON.parse(raw) as KernelStepJson
}

/** Camel-case an `entropy_sample` kernel observation into the SDK's `EntropySample`. */
export function entropySampleFromObservation(obs: KernelObservation): EntropySample {
  return {
    turn: obs.turn ?? 0,
    score: obs.score ?? 0,
    scoreVersion: obs.score_version ?? 0,
    rho: obs.rho ?? 0,
    repeatPressure: obs.repeat_pressure ?? 0,
    failureRate: obs.failure_rate ?? 0,
    rollbacksInWindow: obs.rollbacks_in_window ?? 0,
    windowTurns: obs.window_turns ?? 0,
  }
}

export function kernelMessageToSdk(raw: Record<string, unknown>): Message {
  const content = raw.content
  const canonicalParts = typeof content === "string"
    ? decodeCanonicalContentParts(content)
    : undefined
  const structuredContent = canonicalParts ?? (Array.isArray(content) ? content : undefined)
  const message: Message = {
    role: raw.role as Message["role"],
    content: canonicalParts
      ? canonicalParts
          .filter(part => part.type === "text")
          .map(part => String(part.text ?? ""))
          .join("")
      : typeof content === "string"
      ? content
      : Array.isArray(content)
        ? content
            .filter((part): part is Record<string, unknown> => {
              return typeof part === "object" && part !== null && part.type === "text"
            })
            .map(part => String(part.text ?? ""))
            .join("")
        : "",
    toolCalls: ((raw.tool_calls as Array<Record<string, unknown>>) ?? []).map(tc => ({
      id: String(tc.call_id ?? tc.id ?? ""),
      name: String(tc.name ?? ""),
      arguments: JSON.stringify(tc.arguments ?? {}),
    })),
  }
  if (typeof (raw.tokens ?? raw.token_count) === "number") {
    message.tokenCount = Number(raw.tokens ?? raw.token_count)
  }
  if (structuredContent) {
    message.contentParts = structuredContent
      .filter((part): part is Record<string, unknown> => typeof part === "object" && part !== null)
      .map(part => {
        if (part.type === "text") {
          return { type: "text", text: String(part.text ?? "") }
        }
        if (part.type === "tool_result") {
          return {
            type: "tool_result",
            callId: String(part.call_id ?? ""),
            output: String(part.output ?? ""),
            isError: Boolean(part.is_error),
          }
        }
        if (part.type === "image") {
          return {
            type: "image",
            url: part.url as string | undefined,
            data: part.data as string | undefined,
            mediaType: part.media_type as string | undefined,
            detail: part.detail as "auto" | "low" | "high" | undefined,
          }
        }
        if (part.type === "audio") {
          return {
            type: "audio",
            data: String(part.data ?? ""),
            mediaType: String(part.media_type ?? "audio/wav"),
          }
        }
        return { type: "text", text: "" }
      })
  } else if (typeof raw.tool_call_id === "string") {
    message.contentParts = [{
      type: "tool_result",
      callId: raw.tool_call_id,
      output: message.content,
      isError: false,
    }]
  }
  return message
}

export function renderedContextToSdk(raw: Record<string, unknown>): RenderedContext {
  const rawStateTurn = (raw.state_turn ?? raw.stateTurn) as Record<string, unknown> | undefined
  const frozenLen = (raw.frozen_prefix_len ?? raw.frozenPrefixLen) as number | undefined
  const ctx: RenderedContext = {
    systemText: String(raw.system_text ?? raw.systemText ?? ""),
    systemStable: String(raw.system_stable ?? raw.systemStable ?? ""),
    systemKnowledge: String(raw.system_knowledge ?? raw.systemKnowledge ?? ""),
    turns: ((raw.turns as Array<Record<string, unknown>>) ?? []).map(kernelMessageToSdk),
  }
  if (rawStateTurn) ctx.stateTurn = kernelMessageToSdk(rawStateTurn)
  if (typeof frozenLen === "number") ctx.frozenPrefixLen = frozenLen
  return ctx
}

function mapKernelAction(raw: Record<string, unknown>): KernelRunnerAction {
  const effectId = String(raw.effect_id ?? "")
  if (!effectId) throw new Error(`kernel action ${String(raw.kind)} is missing effect_id`)
  switch (raw.kind) {
    case "call_provider":
      return {
        kind: "call_provider",
        effectId,
        context: renderedContextToSdk((raw.context as Record<string, unknown>) ?? {}),
        tools: ((raw.tools as Array<Record<string, unknown>>) ?? []).map(t => ({
          name: String(t.name ?? ""),
          description: String(t.description ?? ""),
          parameters: JSON.stringify(t.parameters ?? {}),
        })),
      }
    case "execute_tool":
      return {
        kind: "execute_tool",
        effectId,
        calls: ((raw.calls as Array<Record<string, unknown>>) ?? []).map(c => ({
          id: String(c.id ?? ""),
          name: String(c.name ?? ""),
          arguments: JSON.stringify(c.arguments ?? {}),
        })),
      }
    case "request_approval":
      return {
        kind: "request_approval",
        effectId,
        requests: ((raw.requests as Array<Record<string, unknown>>) ?? []).map(request => ({
          callId: String(request.call_id ?? ""),
          tool: String(request.tool ?? ""),
          arguments: JSON.stringify(request.arguments ?? {}),
          reason: String(request.reason ?? ""),
        })),
      }
    case "spawn_workflow":
      return {
        kind: "spawn_workflow",
        effectId,
        nodes: (raw.nodes as Array<Record<string, unknown>>) ?? [],
        ...(raw.budget && typeof raw.budget === "object"
          ? { budget: raw.budget as Record<string, unknown> }
          : {}),
      }
    case "preempt_sub_agents":
      return {
        kind: "preempt_sub_agents",
        effectId,
        agentIds: (raw.agent_ids as string[]) ?? [],
        reason: String(raw.reason ?? ""),
      }
    case "persist_memory":
      return {
        kind: "persist_memory",
        effectId,
        memory: (raw.memory as Record<string, unknown>) ?? {},
      }
    case "query_memory":
      return {
        kind: "query_memory",
        effectId,
        query: (raw.query as Record<string, unknown>) ?? {},
        requestedK: Number(raw.requested_k ?? 0),
      }
    case "spool_large_result":
      return {
        kind: "spool_large_result",
        effectId,
        callId: String(raw.call_id ?? ""),
        tool: String(raw.tool ?? ""),
        output: String(raw.output ?? ""),
        originalSize: Number(raw.original_size ?? 0),
        previewSize: Number(raw.preview_size ?? 0),
      }
    case "archive_page_out":
      return {
        kind: "archive_page_out",
        effectId,
        turn: Number(raw.turn ?? 0),
        action: String(raw.action ?? "auto_compact"),
        ...(typeof raw.summary === "string" ? { summary: raw.summary } : {}),
        archived: ((raw.archived as Array<Record<string, unknown>>) ?? []).map(kernelMessageToSdk),
        tier: String(raw.tier ?? "durable"),
      }
    case "evaluate_milestone":
      return {
        kind: "evaluate_milestone",
        effectId,
        phaseId: String(raw.phase_id ?? ""),
        criteria: (raw.criteria as string[]) ?? [],
        verifier: raw.verifier as MilestoneVerifierKind | undefined,
        requiredEvidence: (raw.required_evidence as string[]) ?? [],
      }
    case "done": {
      const result = (raw.result as Record<string, unknown>) ?? {}
      const pace = result.pace_decision as
        | { action?: string; delay_ms?: number; reason?: string; coerced_from?: string }
        | undefined
      return {
        kind: "done",
        effectId,
        result: {
          termination: String(result.termination ?? "error"),
          turnsUsed: Number(result.turns_used ?? 0),
          totalTokensUsed: Number(result.total_tokens_used ?? 0),
          // ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs).
          ...(pace
            ? {
                paceDecision: {
                  action: (pace.action ?? "stop") as "continue" | "sleep" | "stop",
                  delayMs: pace.delay_ms,
                  reason: pace.reason ?? "",
                  coercedFrom: pace.coerced_from,
                },
              }
            : {}),
        },
      }
    }
    default:
      throw new Error(`unknown KernelAction kind: ${String(raw.kind)}`)
  }
}

function stepInput(runtime: KernelRuntimeHandle, event: Record<string, unknown>): string {
  let state = kernelWireStates.get(runtime)
  if (!state) {
    // Globally unique, never a process-local counter: durable session logs key the kernel
    // genesis/transaction chains by (sessionId, operationId) and outlive this process, so a
    // counter that restarts at 1 collides with yesterday's chain on the same session (genesis
    // digest conflict, or step_seq successor violation when the policy digest happens to match).
    state = {
      operationId: `node-operation-${crypto.randomUUID()}`,
      nextEventSequence: 1,
    }
    kernelWireStates.set(runtime, state)
  }
  const correlatedEvent = event.kind === "cancel_operation"
    ? { ...event, operation_id: state.operationId }
    : event
  return JSON.stringify({
    version: KERNEL_ABI_VERSION,
    operation_id: state.operationId,
    event_id: `${state.operationId}-event-${state.nextEventSequence++}`,
    observed_at_ms: Date.now(),
    event: correlatedEvent,
  })
}

interface DurableKernelState {
  sessionId: string
  operationId: string
  genesisDigest: string
  /**
   * The runtime's genesis-shape checkpoint, captured before its first transition. Replaying it
   * through `restore()` swaps the binding's inner runtime *in place*, so this handle returns to the
   * fresh shape `rebuildKernelRuntime` demands without invalidating the host's reference to it.
   */
  genesisSnapshotJson: string
}

const durableKernelStates = new WeakMap<KernelRuntimeHandle, DurableKernelState>()

/**
 * Spec §8.3 rows 5/6: once a record is durable, `abort` is no longer a legal disposal — the runtime
 * is what gets thrown away, and the authoritative state is re-derived from the journal. This error
 * is that instruction, made explicit for the host.
 */
export class DurableKernelRebuildRequiredError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options)
    this.name = "DurableKernelRebuildRequiredError"
  }
}

/**
 * A runtime that reached an "already durable, cannot publish" state. Fail closed: it is out of sync
 * with the journal, so every later transition on it is rejected with the original reason instead of
 * silently continuing on a runtime the protocol says must be discarded.
 */
const discardedKernelRuntimes = new WeakMap<KernelRuntimeHandle, DurableKernelRebuildRequiredError>()

function discardKernelRuntime(
  runtime: KernelRuntimeHandle,
  message: string,
  cause: unknown,
): DurableKernelRebuildRequiredError {
  const error = new DurableKernelRebuildRequiredError(message, { cause })
  discardedKernelRuntimes.set(runtime, error)
  return error
}

/**
 * Exactly one CAS conflict is retried per input. Rebuild is O(journal length), and a *second*
 * consecutive conflict on the same (session, operation) chain is not a transient race — it means two
 * live writers own one operation, which is a host identity error that must surface rather than be
 * ground down by an unbounded retry loop.
 */
const DURABLE_CAS_REBUILD_RETRIES = 1

function isKernelCasConflict(error: unknown): boolean {
  if (error instanceof KernelLogConflictError) return true
  // Hosts may supply their own journal (§9.4) and dual-package loads defeat `instanceof`; the two
  // class names are part of the documented conflict contract.
  const name = (error as { name?: unknown } | null | undefined)?.name
  return name === "KernelLogConflictError" || name === "JournalCasConflictError"
}

/**
 * Execute one production transition behind the durable action-publish gate. Genesis is persisted
 * before prepare; a newly prepared transaction is CAS-appended before commit; only the committed
 * step is returned to callers, so actions and observations cannot escape early.
 */
export async function durableKernelStep(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  sessionId: string,
  event: Record<string, unknown>,
): Promise<KernelStepJson> {
  const discarded = discardedKernelRuntimes.get(runtime)
  if (discarded) throw discarded

  const inputJson = stepInput(runtime, event)
  const input = JSON.parse(inputJson) as Record<string, unknown>
  const operationId = String(input.operation_id ?? "")
  if (!operationId) throw new Error("kernel input is missing operation_id")

  let durableState = durableKernelStates.get(runtime)
  if (!durableState) {
    // Captured before the first transition, i.e. while the runtime is still genesis-fresh — this is
    // both the genesis body and the reset point every later rebuild replays from.
    const genesisSnapshotJson = runtime.snapshot()
    const snapshot = JSON.parse(genesisSnapshotJson) as KernelSnapshot
    const genesis = await createKernelOperationGenesis({
      abi_version: KERNEL_ABI_VERSION,
      operation_id: operationId,
      initial_scheduler_policy: snapshot.initial_policy as unknown as Record<string, unknown>,
      resolved_runtime_defaults: {
        snapshot_version: snapshot.snapshot_version,
        snapshot_input_limit: snapshot.snapshot_input_limit,
        max_input_bytes: snapshot.max_input_bytes,
        snapshot_journal_bytes_limit: snapshot.snapshot_journal_bytes_limit,
      },
      default_policy_version: 1,
    })
    const receipt = await sessionLog.appendKernelGenesis(sessionId, genesis)
    durableState = {
      sessionId,
      operationId,
      genesisDigest: receipt.genesis_digest,
      genesisSnapshotJson,
    }
    durableKernelStates.set(runtime, durableState)
  } else if (durableState.sessionId !== sessionId || durableState.operationId !== operationId) {
    throw new Error("kernel runtime cannot change its durable session or operation identity")
  }

  return durableKernelTransition(
    runtime,
    sessionLog,
    durableState,
    inputJson,
    DURABLE_CAS_REBUILD_RETRIES,
  )
}

/**
 * One prepare/append/commit attempt, plus the §8.3 failure matrix.
 *
 * `appended` is the abort boundary and it flips on the line immediately after the CAS append returns
 * — **before** `commitPrepared`. Everything downstream of a successful append is a durable fact:
 * a commit that throws, a digest mismatch, a lost response. None of them may call `abortPrepared`
 * (row 6: "discard the runtime, rebuild from the journal; never undo the durable record"). Only the
 * pre-append window (rows 3/4) owns a process-local candidate that abort is allowed to drop.
 *
 * A CAS conflict (row 4) is the one recoverable failure: nothing was appended, so the candidate is
 * aborted, the runtime is re-folded from the authoritative record stream, and the *same* input is
 * replayed once against the new head.
 */
async function durableKernelTransition(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  state: DurableKernelState,
  inputJson: string,
  rebuildRetriesLeft: number,
): Promise<KernelStepJson> {
  const prepared = JSON.parse(runtime.prepareStep(inputJson)) as KernelPreparedStep
  if (prepared.status !== "prepared") return prepared.step
  const token = prepared.prepare_token
  if (!token) throw new Error("prepared kernel transition is missing its commit token")
  let appended = false
  try {
    const head = await sessionLog.kernelTransactionHead(state.sessionId, state.operationId)
    if (!head) throw new Error("durable kernel genesis is missing before transaction append")
    const transaction = await createKernelTransaction({
      operation_id: state.operationId,
      step_seq: prepared.step.step_seq,
      base_generation: prepared.base_generation,
      input: prepared.input,
      step: prepared.step as unknown as Record<string, unknown>,
      previous_transaction_digest: head,
    })
    await sessionLog.compareAndAppendKernelTransaction(state.sessionId, head, transaction)
    appended = true
    const committedStep = parseStep(runtime.commitPrepared(token))
    if (kernelRecordDigest(committedStep) !== transaction.step_digest) {
      throw new Error("committed kernel step does not match the durable prepared step")
    }
    return committedStep
  } catch (transitionError) {
    if (appended) {
      throw discardKernelRuntime(
        runtime,
        "durable kernel record was appended but its transition could not be published; " +
          "discard this runtime and rebuild it from the journal — the durable record must stand",
        transitionError,
      )
    }
    try {
      runtime.abortPrepared(token)
    } catch (abortError) {
      throw new AggregateError(
        [transitionError, abortError],
        "durable transition failed and the prepared kernel state could not be aborted",
      )
    }
    if (rebuildRetriesLeft > 0 && isKernelCasConflict(transitionError)) {
      await rebuildKernelRuntimeFromJournal(runtime, sessionLog, state, transitionError)
      return durableKernelTransition(runtime, sessionLog, state, inputJson, rebuildRetriesLeft - 1)
    }
    throw transitionError
  }
}

/**
 * Re-fold this runtime from the authoritative record stream after losing a CAS race (§8.3 row 4).
 *
 * The reset is deliberately in place: `restore()` replaces the binding's inner runtime while the JS
 * handle survives, so the runner's `runtime` reference — and the wire state keyed by that handle,
 * which must keep minting event ids from the same operation identity — stay valid. That is also why
 * this calls `runtime.restore()` directly instead of the exported `restoreKernelRuntime` helper,
 * which would drop the wire state along with the (absent) snapshot operation id.
 */
async function rebuildKernelRuntimeFromJournal(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  state: DurableKernelState,
  conflict: unknown,
): Promise<void> {
  let genesis: KernelOperationGenesis | undefined
  let transactions: KernelTransaction[]
  try {
    genesis = await sessionLog.readKernelGenesis(state.sessionId, state.operationId)
    if (!genesis) {
      throw new KernelLogIntegrityError(
        "durable kernel genesis is missing; the operation cannot be rebuilt after a CAS conflict",
      )
    }
    transactions = (await sessionLog.readKernelTransactions(state.sessionId, state.operationId)).map(
      entry => entry.transaction,
    )
  } catch (readError) {
    // Nothing has been mutated yet, so the runtime is still usable; report both causes.
    throw new AggregateError(
      [conflict, readError],
      "durable kernel CAS conflict could not be rebuilt: the authoritative record stream is unreadable",
    )
  }

  try {
    runtime.restore(state.genesisSnapshotJson)
    rebuildKernelRuntime(runtime, genesis, transactions)
  } catch (rebuildError) {
    // A partially re-folded runtime is unusable, exactly as `rebuildKernelRuntime` documents.
    throw discardKernelRuntime(
      runtime,
      "durable kernel CAS conflict could not be rebuilt from the journal; discard this runtime",
      rebuildError,
    )
  }
}

export async function durableKernelApply(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  sessionId: string,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): Promise<KernelObservation[]> {
  const step = await durableKernelStep(runtime, sessionLog, sessionId, event)
  const fault = step.faults?.[0]
  if (fault) throw new Error(`${fault.code ?? "kernel_fault"}: ${fault.message ?? "kernel transition failed"}`)
  pending.push(...step.observations)
  return step.observations
}

export async function durableKernelMaybeAction(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  sessionId: string,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): Promise<KernelRunnerAction | null> {
  const step = await durableKernelStep(runtime, sessionLog, sessionId, event)
  const fault = step.faults?.[0]
  if (fault) throw new Error(`${fault.code ?? "kernel_fault"}: ${fault.message ?? "kernel transition failed"}`)
  pending.push(...step.observations)
  const raw = step.actions[0]
  return raw ? mapKernelAction(raw) : null
}

export async function durableKernelAction(
  runtime: KernelRuntimeHandle,
  sessionLog: SessionLog,
  sessionId: string,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): Promise<KernelRunnerAction> {
  const action = await durableKernelMaybeAction(runtime, sessionLog, sessionId, pending, event)
  if (!action) throw new Error("kernel transition must return one action")
  return action
}

export function kernelApply(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelObservation[] {
  const step = kernelStep(runtime, event)
  const fault = step.faults?.[0]
  if (fault) throw new Error(`${fault.code ?? "kernel_fault"}: ${fault.message ?? "kernel transition failed"}`)
  pending.push(...step.observations)
  return step.observations
}

export function kernelAction(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelRunnerAction {
  const action = kernelMaybeAction(runtime, pending, event)
  if (!action) throw new Error("kernel transition must return one action")
  return action
}

/**
 * Like {@link kernelAction} but tolerates a zero-action step. Used for events
 * whose outcome may not drive a provider call — e.g. a signal the kernel queues
 * or ignores returns no action. Returns `null` in that case.
 */
export function kernelMaybeAction(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelRunnerAction | null {
  const step = kernelStep(runtime, event)
  const fault = step.faults?.[0]
  if (fault) throw new Error(`${fault.code ?? "kernel_fault"}: ${fault.message ?? "kernel transition failed"}`)
  pending.push(...step.observations)
  const raw = step.actions[0]
  return raw ? mapKernelAction(raw) : null
}

/** Internal ABI-v2 step primitive shared by SDK adapters and conformance tests. */
export function kernelStep(
  runtime: KernelRuntimeHandle,
  event: Record<string, unknown>,
): KernelStepJson {
  return parseStep(runtime.step(stepInput(runtime, event)))
}
