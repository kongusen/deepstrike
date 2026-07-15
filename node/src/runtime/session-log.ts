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

export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>
  appendKernelGenesis(sessionId: string, genesis: KernelOperationGenesis): Promise<KernelGenesisReceipt>
  readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined>
  compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt>
  readKernelTransactions(sessionId: string, operationId: string, fromStepSeq?: number): Promise<KernelTransactionEntry[]>
  kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined>
}

export class InMemorySessionLog implements SessionLog {
  private store = new Map<string, Array<{ seq: number; event: SessionEvent }>>()
  private seqCounters = new Map<string, number>()
  private genesisStore = new Map<string, { log_seq: number; genesis: KernelOperationGenesis }>()
  private transactionStore = new Map<string, KernelTransactionEntry[]>()

  private operationKey(sessionId: string, operationId: string): string {
    return `${sessionId}\u0000${operationId}`
  }

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

  async appendKernelGenesis(
    sessionId: string,
    genesis: KernelOperationGenesis,
  ): Promise<KernelGenesisReceipt> {
    verifyKernelOperationGenesis(genesis)
    const operationKey = this.operationKey(sessionId, genesis.operation_id)
    const existing = this.genesisStore.get(operationKey)
    if (existing) {
      if (existing.genesis.genesis_digest !== genesis.genesis_digest) {
        throw new KernelLogConflictError("session already has a different kernel operation genesis")
      }
      return { log_seq: existing.log_seq, genesis_digest: genesis.genesis_digest }
    }
    const log_seq = this.nextSeq(sessionId)
    this.genesisStore.set(operationKey, { log_seq, genesis })
    return { log_seq, genesis_digest: genesis.genesis_digest }
  }

  async readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined> {
    return this.genesisStore.get(this.operationKey(sessionId, operationId))?.genesis
  }

  async compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt> {
    verifyKernelTransaction(transaction)
    const operationKey = this.operationKey(sessionId, transaction.operation_id)
    const genesis = this.genesisStore.get(operationKey)?.genesis
    if (!genesis) throw new KernelLogIntegrityError("kernel transaction requires a durable genesis")
    if (transaction.operation_id !== genesis.operation_id) {
      throw new KernelLogIntegrityError("kernel transaction operation_id does not match genesis")
    }
    const head = await this.kernelTransactionHead(sessionId, transaction.operation_id)
    if (head !== expectedTransactionHead || transaction.previous_transaction_digest !== head) {
      throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
    }
    const entries = this.transactionStore.get(operationKey) ?? []
    verifyKernelTransactionSuccessor(entries.at(-1)?.transaction, transaction)
    const log_seq = this.nextSeq(sessionId)
    entries.push({ log_seq, transaction })
    this.transactionStore.set(operationKey, entries)
    return { log_seq, transaction_digest: transaction.transaction_digest }
  }

  async readKernelTransactions(
    sessionId: string,
    operationId: string,
    fromStepSeq = 1,
  ): Promise<KernelTransactionEntry[]> {
    return (this.transactionStore.get(this.operationKey(sessionId, operationId)) ?? []).filter(
      entry => entry.transaction.step_seq >= fromStepSeq,
    )
  }

  async kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined> {
    const operationKey = this.operationKey(sessionId, operationId)
    const entries = this.transactionStore.get(operationKey) ?? []
    return entries.at(-1)?.transaction.transaction_digest
      ?? this.genesisStore.get(operationKey)?.genesis.genesis_digest
  }
}

type PersistedSessionRecord =
  | { seq: number; event: SessionEvent }
  | { seq: number; record_type: "kernel_genesis"; genesis: KernelOperationGenesis }
  | { seq: number; record_type: "kernel_transaction"; transaction: KernelTransaction }

// Single-writer per session. Safe for concurrent appends within one instance.
// Cross-instance (multi-process) safety requires an external lock.
export class FileSessionLog implements SessionLog {
  // Lazy-initialized per-session counter. Avoids re-reading the file on every append.
  private seqCounters = new Map<string, number>()
  private readonly appends = new KeyedSerialExecutor()

  constructor(private dir: string) {}

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

  async appendKernelGenesis(
    sessionId: string,
    genesis: KernelOperationGenesis,
  ): Promise<KernelGenesisReceipt> {
    return this.appends.run(sessionId, async () => {
      verifyKernelOperationGenesis(genesis)
      const existing = (await this.readRecords(sessionId)).find(
        (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_genesis" }> =>
          "record_type" in record
          && record.record_type === "kernel_genesis"
          && record.genesis.operation_id === genesis.operation_id,
      )
      if (existing) {
        if (existing.genesis.genesis_digest !== genesis.genesis_digest) {
          throw new KernelLogConflictError("session already has a different kernel operation genesis")
        }
        return { log_seq: existing.seq, genesis_digest: genesis.genesis_digest }
      }
      const log_seq = await this.nextSeq(sessionId)
      await this.appendRecord(sessionId, {
        seq: log_seq,
        record_type: "kernel_genesis",
        genesis,
      })
      return { log_seq, genesis_digest: genesis.genesis_digest }
    })
  }

  async readKernelGenesis(sessionId: string, operationId: string): Promise<KernelOperationGenesis | undefined> {
    const record = (await this.readRecords(sessionId)).find(
      (entry): entry is Extract<PersistedSessionRecord, { record_type: "kernel_genesis" }> =>
        "record_type" in entry
        && entry.record_type === "kernel_genesis"
        && entry.genesis.operation_id === operationId,
    )
    return record?.genesis
  }

  async compareAndAppendKernelTransaction(
    sessionId: string,
    expectedTransactionHead: string,
    transaction: KernelTransaction,
  ): Promise<DurableAppendReceipt> {
    return this.appends.run(sessionId, async () => {
      verifyKernelTransaction(transaction)
      const records = await this.readRecords(sessionId)
      const genesisRecord = records.find(
        (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_genesis" }> =>
          "record_type" in record
          && record.record_type === "kernel_genesis"
          && record.genesis.operation_id === transaction.operation_id,
      )
      if (!genesisRecord) throw new KernelLogIntegrityError("kernel transaction requires a durable genesis")
      if (transaction.operation_id !== genesisRecord.genesis.operation_id) {
        throw new KernelLogIntegrityError("kernel transaction operation_id does not match genesis")
      }
      const transactions = records.filter(
        (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_transaction" }> =>
          "record_type" in record
          && record.record_type === "kernel_transaction"
          && record.transaction.operation_id === transaction.operation_id,
      )
      const head = transactions.at(-1)?.transaction.transaction_digest
        ?? genesisRecord.genesis.genesis_digest
      if (head !== expectedTransactionHead || transaction.previous_transaction_digest !== head) {
        throw new KernelLogConflictError("kernel transaction head changed before compare-and-append")
      }
      verifyKernelTransactionSuccessor(transactions.at(-1)?.transaction, transaction)
      const log_seq = await this.nextSeq(sessionId)
      await this.appendRecord(sessionId, {
        seq: log_seq,
        record_type: "kernel_transaction",
        transaction,
      })
      return { log_seq, transaction_digest: transaction.transaction_digest }
    })
  }

  async readKernelTransactions(
    sessionId: string,
    operationId: string,
    fromStepSeq = 1,
  ): Promise<KernelTransactionEntry[]> {
    return (await this.readRecords(sessionId))
      .filter(
        (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_transaction" }> =>
          "record_type" in record
          && record.record_type === "kernel_transaction"
          && record.transaction.operation_id === operationId
          && record.transaction.step_seq >= fromStepSeq,
      )
      .map(record => ({ log_seq: record.seq, transaction: record.transaction }))
  }

  async kernelTransactionHead(sessionId: string, operationId: string): Promise<string | undefined> {
    const records = await this.readRecords(sessionId)
    const transaction = records.filter(
      (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_transaction" }> =>
        "record_type" in record
        && record.record_type === "kernel_transaction"
        && record.transaction.operation_id === operationId,
    ).at(-1)
    if (transaction) return transaction.transaction.transaction_digest
    return records.find(
      (record): record is Extract<PersistedSessionRecord, { record_type: "kernel_genesis" }> =>
        "record_type" in record
        && record.record_type === "kernel_genesis"
        && record.genesis.operation_id === operationId,
    )?.genesis.genesis_digest
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
