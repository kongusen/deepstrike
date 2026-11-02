import { SESSION_EVENT_KINDS as REGISTERED_SESSION_EVENT_KINDS, SESSION_EVENT_REGISTRY } from "./session-event-registry.generated.js"
import type { ProviderReplay, ProviderWireEvidence, ToolCall, ToolErrorKind } from "../types.js"
import type { MemoryRecall, MemoryScope } from "../memory/index.js"
import type { KernelPrimitive } from "./kernel-event-log.js"
import { primitiveForKind } from "./kernel-event-log.js"
import type { KernelJournal } from "./kernel-journal.js"
import { InMemoryKernelJournal } from "./kernel-journal.js"
import type { NormalizedProviderUsage, RecordedPromptMeasurement, ResolvedProviderRoute } from "../providers/request-plan.js"

export type RollbackReason =
  | { kind: "fatal_tool_error"; tool_name: string; error: string }
  | { kind: "governance_denied"; tool_name: string; reason: string }
  | { kind: "provider_failure"; error: string }
  | { kind: "timeout" }
  | { kind: "user_interrupt" }
  | { kind: "malformed_replay"; reason: string }

/**
 * P4 §3 mirror: the SessionLog wire payload of a ProviderAttempt (P4 §1.2 fields, flattened,
 * snake_case). Nested objects keep their native shape. The kernel-minted effect_id joins this
 * host evidence to the journal effect chain 1:1 (C6); step_seq NEVER enters SessionLog (P2 §4).
 */
export interface ProviderAttemptRecord {
  effect_id: string
  attempt_seq: number
  route: ResolvedProviderRoute
  request_fingerprint: string
  status: "success" | "transport_exhausted" | "aborted" | "rejected"
  transport_rungs: number
  last_error_class?: string
  started_at_ms: number
  finished_at_ms: number
  usage?: NormalizedProviderUsage
  wire_evidence?: ProviderWireEvidence
  accounting_policy_id?: string
}

/** One knowledge item's lifecycle in a `context_lifecycle_reported` event (plan task 4.1). */
export interface ContextItemRecord {
  key?: string
  origin: "system" | "skill" | "memory_recall" | "reference" | "tool_result" | "dependency_output" | "user" | "assistant" | "task_state" | "signal" | "provider_summary"
  tokens: number
  value: {
    injected_turn: number
    ref_count: number
    last_referenced_step?: number
    last_referenced_turn?: number
    expanded?: boolean
    paged_in_after_evict?: boolean
  }
  /** Why it left; absent while resident. */
  end?: "evicted" | "expired" | "removed"
}

export type SessionEvent =
  // P4-S1: `route` is the runner-construction ResolvedProviderRoute snapshot (P4 §0.2).
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string; attachments?: import("../types.js").ContentPart[]; route?: ResolvedProviderRoute; metadata?: Record<string, unknown> }
  // P3-S2 + P4-S1: effect_id (G4) + invocation_id + wire_evidence (D1). The wire evidence bundle
  // is the sole persisted carrier for provider replay state.
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; effect_id?: string; invocation_id?: string; wire_evidence?: ProviderWireEvidence }
  | { kind: "prompt_measured"; turn: number; measurement: RecordedPromptMeasurement; effect_id?: string }
  // P4-S1 (G1): one record per provider attempt — the full P4 §1.2 payload.
  | ({ kind: "provider_attempt" } & ProviderAttemptRecord)
  | { kind: "tool_requested"; turn: number; calls: ToolCall[] }
  | { kind: "tool_completed"; turn: number; results: Array<{ call_id: string; output: string; is_error?: boolean; is_fatal?: boolean; error_kind?: ToolErrorKind; token_count?: number; content: { blocks: Record<string, unknown>[] } }>; effect_id?: string }
  | { kind: "tool_argument_repaired"; turn: number; tool: string; original_arguments: string; repaired_arguments: string }
  | { kind: "tool_denied"; turn: number; call_id: string; tool_name: string; reason: string }
  | { kind: "permission_requested"; turn: number; tool: string; arguments: string; reason?: string }
  | { kind: "permission_resolved"; turn: number; approved: boolean; responder: string }
  | {
      kind: "compressed"
      turn: number
      archived_seq_range: [number, number]
      action?: "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
      summary?: string
      summary_tokens?: number
      preserved_refs?: string[]
    }
  | {
      kind: "page_out"
      turn: number
      action?: "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
      summary?: string
      tier_hint?: string
      message_count?: number
      archive_ref?: string
    }
  | { kind: "semantic_archive_pending"; effect_id: string; action?: string }
  | { kind: "semantic_archive_completed"; effect_id: string; record_id: string }
  | { kind: "semantic_archive_failed"; effect_id: string; error: string }
  | { kind: "page_in"; turn: number; entry_count: number }
  | { kind: "rollbacked"; turn: number; checkpoint_history_len: number; reason?: RollbackReason }
  | { kind: "capability_changed"; turn: number; added: string[]; removed: string[]; change_kind?: string; capability_id?: string; version?: string; mounted_by?: string; mount_reason?: string }
  | { kind: "context_renewed"; turn: number; sprint: number; handoff_ref: string }
  | {
      kind: "context_lifecycle_reported"
      turn: number
      knowledge_entries: number
      knowledge_referenced: number
      regret: number
      waste_tokens: number
      cache_loss_tokens: number
      /** Memory records the kernel recalled, and how many the model read with `memory(record_id)`. */
      recalled: number
      recall_expanded: number
      /** Every knowledge item of the run. Absent when the run had none. */
      items?: ContextItemRecord[]
    }
  | { kind: "suspended"; turn: number; reason: string; pending_calls?: string[] }
  | { kind: "resumed"; turn: number; approved?: string[]; denied?: string[] }
  | { kind: "tool_gated"; turn: number; call_id: string; tool: string; reason: string }
  | {
      kind: "signal_delivery_disposed"
      turn: number
      operation_id: string
      delivery_id: string
      attempt: number
      signal_id: string
      disposition: string
      queue_depth: number
    }
  | { kind: "budget_exceeded"; turn: number; operation_id: string; reservation_id?: string; budget: string }
  | { kind: "budget_usage_reported"; turn: number; operation_id: string; reservation_id: string; tokens: number; subagents: number; rounds: number }
  | { kind: "operation_cancelled"; turn: number; operation_id: string; reason: "user" | "deadline" | "lease_lost" | "host_shutdown"; pending_call_ids: string[] }
  | { kind: "milestone_advanced"; turn: number; phase_id: string; capabilities_unlocked: string[] }
  | { kind: "milestone_blocked"; turn: number; phase_id: string; reason: string }
  | { kind: "checkpoint_taken"; turn: number; history_len: number }
  | {
      kind: "entropy_sample"
      turn: number
      score: number
      rho: number
      repeat_pressure: number
      failure_rate: number
      rollbacks_in_window: number
      window_turns: number
    }
  | { kind: "entropy_alert"; turn: number; score: number; threshold: number }
  | {
      kind: "agent_process_changed"
      turn: number
      agent_id: string
      parent_task_id?: string
      /** Host audit identity; canonical kernel observations never populate it. */
      parent_session_id?: string
      role: string
      isolation: string
      context_inheritance: string
      state?: string
      permitted_capability_ids: string[]
      result_termination?: string
    }
  | { kind: "memory_written"; turn: number; record_id: string; scope: MemoryScope; memory_kind: string; name: string; size_bytes: number }
  | { kind: "memory_queried"; turn: number; scope: MemoryScope; query: string; requested_k: number; requires_async_response: boolean }
  | { kind: "memory_validation_failed"; turn: number; record_id: string; error: string }
  | { kind: "memory_write_failed"; turn: number; record_id: string; error: string }
  /** M2 · a write handed to the candidate sink and held for review: nothing is remembered yet.
   *  `receipt_id` is absent when the fact comes from the kernel's own observation. */
  | { kind: "memory_candidate_pending"; turn: number; candidate_id: string; source: string; name: string; receipt_id?: string }
  /** M2 · the candidate sink refused the write. */
  | { kind: "memory_candidate_rejected"; turn: number; candidate_id: string; source: string; name: string; receipt_id: string; reason_code: string }
  | { kind: "memory_query_failed"; turn: number; scope: MemoryScope; query: string; error: string }
  | { kind: "memory_retrieval_result"; hits: MemoryRecall[] }
  | {
      kind: "workflow_node_completed"
      turn: number
      agent_id: string
      status: import("./types/agent.js").WorkflowNodeStatus
      termination: string
      output?: import("../types.js").ModelMessage
    }
  | {
      kind: "workflow_nodes_submitted"
      turn: number
      /** Kernel-shape (snake_case) submitted node specs — persisted so resume can re-apply them. */
      nodes: Record<string, unknown>[]
      /** R3-1: graph base index the batch was appended at (from the kernel's
       *  WorkflowNodesSubmitted observation) — lets resume rebuild exact indices. */
      base_index?: number
      /** W-N3: the submitting node's agent id (absent = host/bootstrap). Resume DROPS batches whose
       *  submitter re-runs — it will re-submit — instead of duplicating their nodes. */
      submitter_agent_id?: string
    }
  | {
      kind: "workflow_batch_spawned"
      turn: number
      node_count: number
      node_ids: string[]
    }
  | {
      kind: "workflow_completed"
      turn: number
      node_outcomes: import("./types/agent.js").KernelWorkflowNodeOutcome[]
      total_nodes: number
    }
  | {
      kind: "kernel_observation"
      turn: number
      observation_kind: string
      raw: Record<string, unknown>
    }
  | { kind: "run_terminal"; reason: string; turns_used: number; total_tokens: number }
  | { kind: "summary_upgraded"; compressed_seq: number; summary: string }
  // L1 (RunGroup): group-ledger events, appended under a group-anchor key (= the group id) so the
  // governance domain's cumulative budget + membership (lineage) persist and rebuild by fold-on-read.
  | { kind: "group_member_joined"; session_id: string; role?: string; member_kind?: "peer" | "vehicle" }
  | { kind: "group_budget_charged"; tokens: number; subagents: number; rounds?: number }
  | {
      kind: "round_started"
      /** 1-based round number within the loop. */
      round: number
      goal: string
    }
  | {
      kind: "round_paced"
      round: number
      action: "continue" | "sleep" | "stop"
      delay_ms?: number
      /** Absolute wake time for sleep — lets a stateless host re-arm from the log alone. */
      wake_at_ms?: number
      reason: string
      coerced_from?: string
    }

export type SessionEventKind = SessionEvent["kind"]

/**
 * The registered session-event vocabulary (F9 / S3, P7-S4). The authority is
 * `contracts/session-events.json`; `session-event-registry.generated.ts` is generated from it
 * (`node scripts/gen-session-event-registry.mjs`). The assertions below fail compilation when the
 * `SessionEvent` union and the registry disagree on a kind or on any kind's required/optional
 * top-level fields, so a new event cannot ship without its registry entry.
 */
export const SESSION_EVENT_KINDS = REGISTERED_SESSION_EVENT_KINDS
export { SESSION_EVENT_REGISTRY }

type SessionEventOf<K> = Extract<SessionEvent, { kind: K }>
type OptionalFieldsOf<T> = { [K in keyof T]-?: {} extends Pick<T, K> ? K : never }[keyof T]
type RequiredFieldsOf<T> = Exclude<keyof T, OptionalFieldsOf<T> | "kind">
type SameKeys<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false
type RegistryOf = typeof SESSION_EVENT_REGISTRY
/** Kinds whose union member disagrees with the registry (never = in lockstep). */
type SessionEventRegistryMismatch = {
  [K in SessionEventKind]: K extends keyof RegistryOf
    ? SameKeys<RequiredFieldsOf<SessionEventOf<K>>, RegistryOf[K]["required"][number]> extends true
      ? SameKeys<OptionalFieldsOf<SessionEventOf<K>>, RegistryOf[K]["optional"][number]> extends true
        ? never
        : K
      : K
    : K
}[SessionEventKind] | Exclude<keyof RegistryOf, SessionEventKind>
const _sessionEventRegistryInLockstep: [SessionEventRegistryMismatch] extends [never] ? true : SessionEventRegistryMismatch = true
void _sessionEventRegistryInLockstep

/**
 * The business-projection log (spec §9.2): run started/terminal, stream events, observations,
 * provider/tool presentation, and audit metadata. Canonical durable records live exclusively in
 * `KernelJournal` (spec §9.1).
 */
export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>
}

/**
 * **Single-isolate dev/test implementation** of both capabilities (spec §9.4: one class may
 * implement several capabilities; the *interfaces* stay separate). Its `KernelJournal` half is
 * `InMemoryKernelJournal`, whose CAS is atomic within one isolate only. A host whose journal must
 * outlive its isolate — or be shared with another one — injects a `DriverKernelJournal` over a
 * durable `JournalStorageDriver` instead.
 */
export class InMemorySessionLog implements SessionLog {
  private store = new Map<string, Array<{ seq: number; event: SessionEvent }>>()
  /** Business event sequence space only — journal records number themselves by `step_seq`. */
  private seqCounters = new Map<string, number>()
  /** The durable transaction capability, held rather than inherited (spec §9.1/§9.4). */
  readonly kernelJournal: KernelJournal = new InMemoryKernelJournal()

  private nextSeq(sessionId: string): number {
    const seq = this.seqCounters.get(sessionId) ?? 0
    this.seqCounters.set(sessionId, seq + 1)
    return seq
  }

  async append(sessionId: string, event: SessionEvent): Promise<number> {
    if (!this.store.has(sessionId)) this.store.set(sessionId, [])
    const entries = this.store.get(sessionId)!
    const seq = this.nextSeq(sessionId)
    entries.push({ seq, event })
    return seq
  }

  async read(sessionId: string, fromSeq = 0, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>> {
    const entries = this.store.get(sessionId) ?? []
    return entries.filter(e => {
      if (e.seq < fromSeq) return false
      if (primitiveFilter && primitiveForKind(e.event.kind) !== primitiveFilter) return false
      return true
    })
  }

  async latestSeq(sessionId: string): Promise<number> {
    return (this.seqCounters.get(sessionId) ?? 0) - 1
  }

}

const PROVIDER_ATTEMPT_KEYS = new Set([
  "kind", "effect_id", "attempt_seq", "route", "request_fingerprint", "status", "transport_rungs",
  "last_error_class", "started_at_ms", "finished_at_ms", "usage", "wire_evidence", "accounting_policy_id",
])
const PROVIDER_ATTEMPT_STATUSES = new Set(["success", "transport_exhausted", "aborted", "rejected"])
const WIRE_EVIDENCE_KEYS = new Set(["protocol", "request_fingerprint", "response_id", "raw_usage", "replay_state"])

function assertCanonicalRoute(value: unknown): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("route must be an object")
  const route = value as Record<string, unknown>
  for (const field of ["routeId", "provider", "protocol", "model", "adapterVersion", "capabilitiesRef"]) {
    if (typeof route[field] !== "string" || (route[field] as string).length === 0) {
      throw new Error(`route ${field} must be a non-empty string`)
    }
  }
  if (!route.endpoint || typeof route.endpoint !== "object" || Array.isArray(route.endpoint)) {
    throw new Error("route endpoint must be an object")
  }
}

function assertCanonicalWireEvidence(value: unknown): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("wire evidence must be an object")
  const evidence = value as Record<string, unknown>
  for (const key of Object.keys(evidence)) if (!WIRE_EVIDENCE_KEYS.has(key)) throw new Error(`wire evidence has unknown field ${key}`)
  if (typeof evidence.protocol !== "string" || evidence.protocol.length === 0) throw new Error("wire evidence protocol is required")
  // G2: the fingerprint is mandatory non-empty — it is what binds this evidence to a request plan.
  if (typeof evidence.request_fingerprint !== "string" || evidence.request_fingerprint.length === 0) {
    throw new Error("wire evidence request_fingerprint is required")
  }
}

/**
 * P4 §3 teeth, wasm mirror of the node/python persisted-record validator. The wasm SessionLog is
 * in-memory only (no durable read path where validation would otherwise live), so the teeth are
 * exported for hosts that bridge these events to durable storage — and for the conformance
 * runner, which pins the rejection contract (`invalid_provider_attempt`) across all three
 * JSON-projection SDKs. Same checks and message wording as node `assertCanonicalProviderAttempt`.
 */
export function assertCanonicalProviderAttemptRecord(event: Record<string, unknown>): void {
  for (const key of Object.keys(event)) if (!PROVIDER_ATTEMPT_KEYS.has(key)) throw new Error(`provider_attempt has unknown field ${key}`)
  if (typeof event.effect_id !== "string" || event.effect_id.length === 0) throw new Error("provider_attempt effect_id is required")
  if (!Number.isInteger(event.attempt_seq) || (event.attempt_seq as number) < 1) throw new Error("provider_attempt attempt_seq must be a positive integer")
  assertCanonicalRoute(event.route)
  if (typeof event.request_fingerprint !== "string" || event.request_fingerprint.length === 0) {
    throw new Error("provider_attempt request_fingerprint is required")
  }
  if (typeof event.status !== "string" || !PROVIDER_ATTEMPT_STATUSES.has(event.status)) {
    throw new Error("provider_attempt status must be success|transport_exhausted|aborted|rejected")
  }
  if (!Number.isInteger(event.transport_rungs) || (event.transport_rungs as number) < 0) {
    throw new Error("provider_attempt transport_rungs must be a non-negative integer")
  }
  if (event.last_error_class !== undefined && typeof event.last_error_class !== "string") {
    throw new Error("provider_attempt last_error_class must be a string")
  }
  for (const field of ["started_at_ms", "finished_at_ms"]) {
    if (typeof event[field] !== "number" || !Number.isFinite(event[field] as number)) {
      throw new Error(`provider_attempt ${field} must be a finite number`)
    }
  }
  if (event.wire_evidence !== undefined) assertCanonicalWireEvidence(event.wire_evidence)
  if (event.accounting_policy_id !== undefined && typeof event.accounting_policy_id !== "string") {
    throw new Error("provider_attempt accounting_policy_id must be a string")
  }
}
