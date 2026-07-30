import { createReadStream } from "node:fs"
import type { KernelPrimitive } from "./kernel-event-log.js"
import { access, mkdir, open as openFile } from "node:fs/promises"
import { join } from "node:path"
import { createInterface } from "node:readline"
import type { ContentPart, ProviderReplay, ToolCall, ToolErrorKind } from "../types.js"
import type { MemoryRecall, MemoryScope } from "../memory/protocols.js"
import { primitiveForKind } from "./kernel-event-log.js"
import { KeyedSerialExecutor } from "./reliability.js"
import type {
  DurableAppendReceipt,
  KernelGenesisReceipt,
  KernelOperationGenesis,
  KernelTransaction,
} from "./kernel-transaction-log.js"
import {
  KernelLogConflictError,
  KernelLogIntegrityError,
  verifyKernelOperationGenesis,
  verifyKernelTransaction,
  verifyKernelTransactionSuccessor,
} from "./kernel-transaction-log.js"
import type { JournalEntry, JournalRecordInput, KernelJournal } from "./kernel-journal.js"
import {
  FileKernelJournal,
  InMemoryKernelJournal,
  JournalCasConflictError,
} from "./kernel-journal.js"

export interface KernelTransactionEntry {
  log_seq: number
  transaction: KernelTransaction
}

export type RollbackReason =
  | { kind: "fatal_tool_error"; tool_name: string; error: string }
  | { kind: "governance_denied"; tool_name: string; reason: string }
  | { kind: "provider_failure"; error: string }
  | { kind: "timeout" }
  | { kind: "user_interrupt" }
  | { kind: "malformed_replay"; reason: string }

export type SessionEvent =
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string; attachments?: ContentPart[] }
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; provider_replay?: ProviderReplay }
  | { kind: "tool_requested"; turn: number; calls: ToolCall[] }
  | { kind: "tool_completed"; turn: number; results: Array<{ call_id: string; output: string; is_error?: boolean; is_fatal?: boolean; error_kind?: ToolErrorKind; token_count?: number }> }
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
  | { kind: "page_in"; turn: number; entry_count: number }
  | {
      kind: "large_result_spooled"
      turn: number
      call_id: string
      tool: string
      original_size: number
      preview_size: number
      spool_ref?: string
    }
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
      score_version: number
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
      parent_session_id: string
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

/**
 * The business-projection log (spec §9.2): run started/terminal, stream events, observations,
 * provider/tool presentation, audit metadata.
 *
 * The five `*Kernel*` methods below are the **legacy journal face**. The durable transaction
 * capability now lives in its own interface — `KernelJournal` in `./kernel-journal.js` (spec §9.1) —
 * so a custom `SessionLog` is no longer forced to masquerade as a transactional journal (§9.4).
 * These methods remain on `SessionLog` only so existing callers (`kernel-step.ts`) keep compiling;
 * both default implementations now forward them to an internally held `KernelJournal` whose
 * sequence space is separate from business event `seq`.
 */
export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>
  /** @deprecated Use {@link KernelJournal.compareAndAppend} with an empty expected head. */
  appendKernelGenesis(sessionId: string, genesis: KernelOperationGenesis): Promise<KernelGenesisReceipt>
  /** @deprecated Use {@link KernelJournal.readFrom} from step 0. */
  readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined>
  /** @deprecated Use {@link KernelJournal.compareAndAppend}. */
  compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt>
  /** @deprecated Use {@link KernelJournal.readFrom} or {@link KernelJournal.recordsAfter}. */
  readKernelTransactions(sessionId: string, operationId: string, fromStepSeq?: number): Promise<KernelTransactionEntry[]>
  /** @deprecated Use {@link KernelJournal.head}. */
  kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined>
}

/* ------------------------------------------------------------------ *
 * Legacy journal face → KernelJournal adapter
 *
 * The legacy face speaks typed `KernelOperationGenesis` / `KernelTransaction` records; the journal
 * speaks opaque bytes + a digest it was given. The adapter is the translation, and it keeps the
 * *typed* integrity checks (tamper detection, successor continuity) on this side — the journal only
 * ever guarantees chain-of-digest and CAS. Genesis is chain position 0, so genesis and transactions
 * share one uniform record chain, exactly as core models it (§8.1).
 * ------------------------------------------------------------------ */

/**
 * Journals are scoped per operation; the legacy `SessionLog` face is scoped per
 * (session, operation). Exported so a host migrating off the deprecated methods can address
 * the same durable chain through the {@link KernelJournal} capability directly.
 */
export function journalOperationKey(sessionId: string, operationId: string): string {
  return `${sessionId}\u0000${operationId}`
}

function encodeJournalRecord(value: KernelOperationGenesis | KernelTransaction, stepSeq: number): JournalRecordInput {
  return {
    step_seq: stepSeq,
    record_digest: "genesis_digest" in value ? value.genesis_digest : value.transaction_digest,
    record_bytes: new Uint8Array(Buffer.from(JSON.stringify(value), "utf8")),
  }
}

function decodeJournalRecord<T>(entry: JournalEntry): T {
  return JSON.parse(Buffer.from(entry.record_bytes).toString("utf8")) as T
}

async function journalAppendGenesis(
  journal: KernelJournal,
  sessionId: string,
  genesis: KernelOperationGenesis,
): Promise<KernelGenesisReceipt> {
  verifyKernelOperationGenesis(genesis)
  const key = journalOperationKey(sessionId, genesis.operation_id)
  try {
    const receipt = await journal.compareAndAppend(key, undefined, encodeJournalRecord(genesis, 0))
    return { log_seq: receipt.step_seq, genesis_digest: genesis.genesis_digest }
  } catch (err) {
    if (!(err instanceof JournalCasConflictError)) throw err
    // A chain already exists: idempotent for the same genesis, a conflict for a different one.
    const existing = await journalReadGenesis(journal, sessionId, genesis.operation_id)
    if (!existing || existing.genesis_digest !== genesis.genesis_digest) {
      throw new KernelLogConflictError("session already has a different kernel operation genesis")
    }
    return { log_seq: 0, genesis_digest: genesis.genesis_digest }
  }
}

async function journalReadGenesis(
  journal: KernelJournal,
  sessionId: string,
  operationId: string,
): Promise<KernelOperationGenesis | undefined> {
  const entries = await journal.readFrom(journalOperationKey(sessionId, operationId), 0)
  const genesis = entries.find(entry => entry.step_seq === 0)
  return genesis ? decodeJournalRecord<KernelOperationGenesis>(genesis) : undefined
}

async function journalCompareAndAppendTransaction(
  journal: KernelJournal,
  sessionId: string,
  expectedTransactionHead: string,
  transaction: KernelTransaction,
): Promise<DurableAppendReceipt> {
  verifyKernelTransaction(transaction)
  const key = journalOperationKey(sessionId, transaction.operation_id)
  const head = await journal.head(key)
  if (!head) throw new KernelLogIntegrityError("kernel transaction requires a durable genesis")
  if (head.record_digest !== expectedTransactionHead || transaction.previous_transaction_digest !== head.record_digest) {
    throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
  }
  // step 0 is the genesis record, which has no `KernelTransaction` predecessor.
  let previous: KernelTransaction | undefined
  if (head.step_seq > 0) {
    const headEntry = (await journal.readFrom(key, head.step_seq))[0]
    if (!headEntry) {
      // The head resolved from a pruned anchor: the typed successor check cannot run without the
      // predecessor record. The legacy face has no bounded-tail replay, so refuse rather than skip.
      throw new KernelLogIntegrityError(
        "kernel transaction predecessor has been pruned; use the KernelJournal capability directly",
      )
    }
    previous = decodeJournalRecord<KernelTransaction>(headEntry)
  }
  verifyKernelTransactionSuccessor(previous, transaction)
  const receipt = await journal.compareAndAppend(
    key,
    expectedTransactionHead,
    encodeJournalRecord(transaction, transaction.step_seq),
  )
  return { log_seq: receipt.step_seq, transaction_digest: transaction.transaction_digest }
}

async function journalReadTransactions(
  journal: KernelJournal,
  sessionId: string,
  operationId: string,
  fromStepSeq: number,
): Promise<KernelTransactionEntry[]> {
  const entries = await journal.readFrom(
    journalOperationKey(sessionId, operationId),
    Math.max(1, fromStepSeq),
  )
  return entries.map(entry => ({
    log_seq: entry.step_seq,
    transaction: decodeJournalRecord<KernelTransaction>(entry),
  }))
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

  /** @deprecated Legacy journal face — forwards to {@link InMemorySessionLog.kernelJournal}. */
  async appendKernelGenesis(
    sessionId: string,
    genesis: KernelOperationGenesis,
  ): Promise<KernelGenesisReceipt> {
    return journalAppendGenesis(this.kernelJournal, sessionId, genesis)
  }

  /** @deprecated Legacy journal face — forwards to {@link InMemorySessionLog.kernelJournal}. */
  async readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined> {
    return journalReadGenesis(this.kernelJournal, sessionId, operationId)
  }

  /** @deprecated Legacy journal face — forwards to {@link InMemorySessionLog.kernelJournal}. */
  async compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt> {
    return journalCompareAndAppendTransaction(
      this.kernelJournal,
      sessionId,
      expectedTransactionHead,
      transaction,
    )
  }

  /** @deprecated Legacy journal face — forwards to {@link InMemorySessionLog.kernelJournal}. */
  async readKernelTransactions(
    sessionId: string,
    operationId: string,
    fromStepSeq = 1,
  ): Promise<KernelTransactionEntry[]> {
    return journalReadTransactions(this.kernelJournal, sessionId, operationId, fromStepSeq)
  }

  /** @deprecated Legacy journal face — forwards to {@link InMemorySessionLog.kernelJournal}. */
  async kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined> {
    return (await this.kernelJournal.head(journalOperationKey(sessionId, operationId)))?.record_digest
  }
}

/**
 * A persisted line. The two `kernel_*` variants are **read-compat only**: kernel records used to
 * share this file (and its sequence space) with business events; they now live in the separate
 * `FileKernelJournal` under `<dir>/kernel-journal`. Keeping the variants means an older file still
 * parses instead of throwing — it does not migrate those records into the journal.
 */
type PersistedSessionRecord =
  | { seq: number; event: SessionEvent }
  | { seq: number; record_type: "kernel_genesis"; genesis: KernelOperationGenesis }
  | { seq: number; record_type: "kernel_transaction"; transaction: KernelTransaction }

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
      if (!("event" in record) || record.seq < fromSeq) continue
      if (primitiveFilter && primitiveForKind(record.event.kind) !== primitiveFilter) continue
      results.push({ seq: record.seq, event: record.event })
    }
    return results
  }

  async latestSeq(sessionId: string): Promise<number> {
    const records = await this.readRecords(sessionId)
    return records.reduce((latest, record) => Math.max(latest, record.seq), -1)
  }

  /** @deprecated Legacy journal face — forwards to {@link FileSessionLog.kernelJournal}. */
  async appendKernelGenesis(
    sessionId: string,
    genesis: KernelOperationGenesis,
  ): Promise<KernelGenesisReceipt> {
    return journalAppendGenesis(this.kernelJournal, sessionId, genesis)
  }

  /** @deprecated Legacy journal face — forwards to {@link FileSessionLog.kernelJournal}. */
  async readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined> {
    return journalReadGenesis(this.kernelJournal, sessionId, operationId)
  }

  /** @deprecated Legacy journal face — forwards to {@link FileSessionLog.kernelJournal}. */
  async compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt> {
    return journalCompareAndAppendTransaction(
      this.kernelJournal,
      sessionId,
      expectedTransactionHead,
      transaction,
    )
  }

  /** @deprecated Legacy journal face — forwards to {@link FileSessionLog.kernelJournal}. */
  async readKernelTransactions(
    sessionId: string,
    operationId: string,
    fromStepSeq = 1,
  ): Promise<KernelTransactionEntry[]> {
    return journalReadTransactions(this.kernelJournal, sessionId, operationId, fromStepSeq)
  }

  /** @deprecated Legacy journal face — forwards to {@link FileSessionLog.kernelJournal}. */
  async kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined> {
    return (await this.kernelJournal.head(journalOperationKey(sessionId, operationId)))?.record_digest
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
        if (line.trim()) records.push(JSON.parse(line) as PersistedSessionRecord)
      }
    } catch (err: unknown) {
      if ((err as { code?: string }).code !== "ENOENT") throw err
    }
    return records
  }
}
