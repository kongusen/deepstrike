import { createReadStream } from "node:fs"
import type { KernelPrimitive } from "./kernel-event-log.js"
import { access, mkdir, open as openFile } from "node:fs/promises"
import { join } from "node:path"
import { createInterface } from "node:readline"
import type { ContentPart, ProviderReplay, ProviderWireEvidence, ToolCall, ToolErrorKind, ToolOutputBlock } from "../types.js"
import type { RecordedPromptMeasurement, ResolvedProviderRoute } from "../providers/request-plan.js"
import type { ProviderAttemptRecord } from "./execution-evidence.js"
import type { MemoryRecall, MemoryScope } from "../memory/protocols.js"
import { primitiveForKind } from "./kernel-event-log.js"
import { KeyedSerialExecutor } from "./reliability.js"
import type { KernelJournal } from "./kernel-journal.js"
import { FileKernelJournal, InMemoryKernelJournal } from "./kernel-journal.js"
import { decodeDurableContent } from "./durable-content.js"

export type RollbackReason =
  | { kind: "fatal_tool_error"; tool_name: string; error: string }
  | { kind: "governance_denied"; tool_name: string; reason: string }
  | { kind: "provider_failure"; error: string }
  | { kind: "timeout" }
  | { kind: "user_interrupt" }
  | { kind: "malformed_replay"; reason: string }

export type SessionEvent =
  // P4-S1: `route` is the runner-construction ResolvedProviderRoute snapshot (P4 §0.2: one
  // fixed route per run today). Older logs simply lack it — C7 degrades, never fails.
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string; attachments?: ContentPart[]; route?: ResolvedProviderRoute }
  // P3-S2 + P4-S1: `effect_id` (G4) + `invocation_id` (P4 §3) join this evidence projection to
  // the journal effect chain; `wire_evidence` (D1) is the ProviderWireEvidence bundle.
  // `provider_replay` is DEPRECATED — carried unchanged for one full minor, then removed (P3 §3.3).
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; provider_replay?: ProviderReplay; effect_id?: string; invocation_id?: string; wire_evidence?: ProviderWireEvidence }
  | { kind: "prompt_measured"; turn: number; measurement: RecordedPromptMeasurement; effect_id?: string }
  // P4-S1 (G1): one record per provider attempt — the full P4 §1.2 payload. The kernel-minted
  // effect_id joins this host evidence to the journal effect chain 1:1 (C6); step_seq NEVER
  // enters SessionLog (P2 §4).
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
  | { kind: "budget_exceeded"; turn: number; budget: string; operation_id: string; reservation_id?: string }
  | {
      kind: "budget_usage_reported"
      turn: number
      operation_id: string
      reservation_id: string
      tokens: number
      subagents: number
      rounds: number
    }
  | {
      kind: "operation_cancelled"
      turn: number
      operation_id: string
      reason: "user" | "deadline" | "lease_lost" | "host_shutdown"
      pending_call_ids: string[]
    }
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
  | { kind: "memory_query_failed"; turn: number; scope: MemoryScope; query: string; error: string }
  | { kind: "memory_retrieval_result"; hits: MemoryRecall[] }
  | {
      kind: "workflow_node_completed"
      turn: number
      agent_id: string
      status: import("../types/agent.js").WorkflowNodeStatus
      termination: string
      /** W-1: result-borne control signals, persisted so resume replays control flow faithfully —
       *  a classifier re-prunes its rejected branches, a recorded loop stop is honored. */
      classify_branch?: string
      tournament_winner?: string
      loop_continue?: boolean
      output?: import("../types.js").Message
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
      node_outcomes: import("../types/agent.js").KernelWorkflowNodeOutcome[]
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
 * The registered session-event vocabulary (F9 / S3, P7-S4). This list is the single authority
 * the cross-SDK manifest fixture pins: a kind added here without the same-commit update to
 * `tests/fixtures/sdk-conformance/canonical/session-event-vocabulary.json` and the python/wasm
 * vocabularies turns cross-SDK conformance red. Declared in `SessionEvent` union order.
 */
export const SESSION_EVENT_KINDS = [
  "run_started",
  "llm_completed",
  "prompt_measured",
  "provider_attempt",
  "tool_requested",
  "tool_completed",
  "tool_argument_repaired",
  "tool_denied",
  "permission_requested",
  "permission_resolved",
  "compressed",
  "page_out",
  "semantic_archive_pending",
  "semantic_archive_completed",
  "semantic_archive_failed",
  "page_in",
  "rollbacked",
  "capability_changed",
  "context_renewed",
  "suspended",
  "resumed",
  "tool_gated",
  "signal_delivery_disposed",
  "budget_exceeded",
  "budget_usage_reported",
  "operation_cancelled",
  "milestone_advanced",
  "milestone_blocked",
  "checkpoint_taken",
  "entropy_sample",
  "entropy_alert",
  "agent_process_changed",
  "memory_written",
  "memory_queried",
  "memory_validation_failed",
  "memory_write_failed",
  "memory_query_failed",
  "memory_retrieval_result",
  "workflow_node_completed",
  "workflow_nodes_submitted",
  "workflow_batch_spawned",
  "workflow_completed",
  "kernel_observation",
  "run_terminal",
  "summary_upgraded",
  "group_member_joined",
  "group_budget_charged",
  "round_started",
  "round_paced",
] as const satisfies readonly SessionEventKind[]

// Compile-time lockstep: the list above and the union must cover each other exactly.
// `satisfies` rejects list entries the union lacks; this rejects union members the list lacks.
type _AssertVocabularyCoversUnion = Exclude<SessionEventKind, (typeof SESSION_EVENT_KINDS)[number]> extends never ? true : never
const _vocabularyCoversUnion: _AssertVocabularyCoversUnion = true
void _vocabularyCoversUnion

/**
 * The business-projection log (spec §9.2): run started/terminal, stream events, observations,
 * provider/tool presentation, audit metadata.
 *
 * Durable kernel records use the separate `KernelJournal` capability. `SessionLog` contains only
 * business observations and never adapts or reserializes canonical records.
 */
export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>
}

/**
 * **Single-process dev/test implementation** of both capabilities (spec §9.4: one class may
 * implement several capabilities; the *interfaces* stay separate). Its `KernelJournal` half is
 * `InMemoryKernelJournal`, whose CAS is atomic within one process only.
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

type PersistedSessionRecord = { seq: number; event: SessionEvent }

/**
 * File-backed `SessionLog`. Business appends are single-writer per session: safe within one
 * instance, **not** across processes — that limitation is confined to the projection log now.
 *
 * The durable transaction capability is delegated to {@link FileKernelJournal}, which *is*
 * cross-process atomic (spec Task 8b), so the two capabilities no longer share a file, a sequence
 * space, or a concurrency story.
 */
export class FileSessionLog implements SessionLog {
  // Lazy-initialized per-session counter for business events only.
  private seqCounters = new Map<string, number>()
  private readonly appends = new KeyedSerialExecutor()
  /** The durable transaction capability, held rather than inherited (spec §9.1/§9.4). */
  readonly kernelJournal: KernelJournal

  constructor(private dir: string) {
    this.kernelJournal = new FileKernelJournal(join(dir, "kernel-journal"))
  }

  private path(sessionId: string): string {
    return join(this.dir, `${sessionId}.jsonl`)
  }

  private async nextSeq(sessionId: string): Promise<number> {
    if (!this.seqCounters.has(sessionId)) {
      const existing = await this.readRecords(sessionId)
      this.seqCounters.set(
        sessionId,
        existing.reduce((next, record) => Math.max(next, record.seq + 1), 0),
      )
    }
    const seq = this.seqCounters.get(sessionId)!
    this.seqCounters.set(sessionId, seq + 1)
    return seq
  }

  async append(sessionId: string, event: SessionEvent): Promise<number> {
    return this.appends.run(sessionId, async () => {
      const seq = await this.nextSeq(sessionId)
      await this.appendRecord(sessionId, { seq, event })
      return seq
    })
  }

  async read(sessionId: string, fromSeq = 0, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>> {
    const results: Array<{ seq: number; event: SessionEvent }> = []
    for (const record of await this.readRecords(sessionId)) {
      if (record.seq < fromSeq) continue
      if (primitiveFilter && primitiveForKind(record.event.kind) !== primitiveFilter) continue
      results.push({ seq: record.seq, event: record.event })
    }
    return results
  }

  async latestSeq(sessionId: string): Promise<number> {
    const records = await this.readRecords(sessionId)
    return records.reduce((latest, record) => Math.max(latest, record.seq), -1)
  }

  private async appendRecord(sessionId: string, record: PersistedSessionRecord): Promise<void> {
    await mkdir(this.dir, { recursive: true })
    const path = this.path(sessionId)
    let isNewFile = false
    try {
      await access(path)
    } catch {
      isNewFile = true
    }
    const file = await openFile(path, "a")
    try {
      await file.appendFile(`${JSON.stringify(record)}\n`, "utf8")
      await file.sync()
    } finally {
      await file.close()
    }
    if (isNewFile) {
      const directory = await openFile(this.dir, "r")
      try {
        await directory.sync()
      } finally {
        await directory.close()
      }
    }
  }

  private async readRecords(sessionId: string): Promise<PersistedSessionRecord[]> {
    const records: PersistedSessionRecord[] = []
    try {
      const rl = createInterface({
        input: createReadStream(this.path(sessionId)),
        crlfDelay: Infinity,
      })
      for await (const line of rl) {
        if (line.trim()) records.push(decodePersistedSessionRecord(JSON.parse(line)))
      }
    } catch (err: unknown) {
      if ((err as { code?: string }).code !== "ENOENT") throw err
    }
    return records
  }
}

function decodePersistedSessionRecord(value: unknown): PersistedSessionRecord {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("session record must be an object")
  const record = value as Record<string, unknown>
  if (Object.keys(record).some(key => key !== "seq" && key !== "event")) throw new Error("session record has unknown fields")
  if (!Number.isInteger(record.seq) || (record.seq as number) < 0) throw new Error("session record seq must be a non-negative integer")
  if (!record.event || typeof record.event !== "object" || Array.isArray(record.event)) throw new Error("session record event must be an object")
  const event = record.event as Record<string, unknown>
  if (event.kind === "llm_completed" && event.provider_replay !== undefined) {
    assertCanonicalProviderReplay(event.provider_replay)
  }
  if (event.kind === "llm_completed" && event.wire_evidence !== undefined) {
    assertCanonicalWireEvidence(event.wire_evidence)
  }
  if (event.kind === "run_started" && event.route !== undefined) {
    assertCanonicalRoute(event.route)
  }
  if (event.kind === "provider_attempt") {
    assertCanonicalProviderAttempt(event)
  }
  if (event.kind === "tool_completed") {
    if (!Array.isArray(event.results)) throw new Error("tool_completed results must be an array")
    for (const result of event.results) {
      if (!result || typeof result !== "object" || Array.isArray(result)) throw new Error("tool_completed result must be an object")
      const wire = result as Record<string, unknown>
      if ("blocks" in wire) throw new Error("tool_completed result has removed blocks field")
      decodeDurableContent(wire.content)
    }
  }
  return { seq: record.seq as number, event: event as SessionEvent }
}

const REPLAY_KEYS = new Set(["protocol", "provider", "model", "native_blocks", "reasoning_content", "reasoning_details", "native_message", "tool_calls"])

function assertCanonicalProviderReplay(value: unknown): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("provider replay must be an object")
  const replay = value as Record<string, unknown>
  for (const key of Object.keys(replay)) if (!REPLAY_KEYS.has(key)) throw new Error(`provider replay has unknown field ${key}`)
  if (typeof replay.protocol !== "string" || replay.protocol.length === 0) throw new Error("provider replay protocol is required")
}

const WIRE_EVIDENCE_KEYS = new Set(["protocol", "request_fingerprint", "response_id", "raw_usage", "replay_state"])
/** P3 §3.3: raw_usage carries BoundedJson semantics — the 4KB cap is enforced at the boundary. */
const WIRE_EVIDENCE_RAW_USAGE_MAX_BYTES = 4096

function assertCanonicalWireEvidence(value: unknown): void {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("wire evidence must be an object")
  const evidence = value as Record<string, unknown>
  for (const key of Object.keys(evidence)) if (!WIRE_EVIDENCE_KEYS.has(key)) throw new Error(`wire evidence has unknown field ${key}`)
  if (typeof evidence.protocol !== "string" || evidence.protocol.length === 0) throw new Error("wire evidence protocol is required")
  // G2: the fingerprint is mandatory non-empty — it is what binds this evidence to a request plan.
  if (typeof evidence.request_fingerprint !== "string" || evidence.request_fingerprint.length === 0) {
    throw new Error("wire evidence request_fingerprint is required")
  }
  if (evidence.response_id !== undefined && typeof evidence.response_id !== "string") {
    throw new Error("wire evidence response_id must be a string")
  }
  if (evidence.raw_usage !== undefined
    && new TextEncoder().encode(JSON.stringify(evidence.raw_usage)).byteLength > WIRE_EVIDENCE_RAW_USAGE_MAX_BYTES) {
    throw new Error("wire evidence raw_usage exceeds the 4KB BoundedJson cap")
  }
  if (evidence.replay_state !== undefined) assertCanonicalProviderReplay(evidence.replay_state)
}

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

const PROVIDER_ATTEMPT_KEYS = new Set([
  "kind", "effect_id", "attempt_seq", "route", "request_fingerprint", "status", "transport_rungs",
  "last_error_class", "started_at_ms", "finished_at_ms", "usage", "wire_evidence", "accounting_policy_id",
])
const PROVIDER_ATTEMPT_STATUSES = new Set(["success", "transport_exhausted", "aborted", "rejected"])

function assertCanonicalProviderAttempt(event: Record<string, unknown>): void {
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
