import { createWriteStream, createReadStream } from "node:fs"
import type { KernelPrimitive } from "./kernel-event-log.js"
import { mkdir } from "node:fs/promises"
import { join } from "node:path"
import { createInterface } from "node:readline"
import type { ContentPart, ProviderReplay, ToolCall, ToolErrorKind } from "../types.js"
import { primitiveForKind } from "./kernel-event-log.js"

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
      archive_ref?: string
      preserved_refs?: string[]
    }
  | {
      kind: "page_out"
      turn: number
      action?: "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
      summary?: string
      tier_hint?: string
      message_count?: number
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
  | { kind: "signal_disposed"; turn: number; signal_id: string; disposition: string; queue_depth: number }
  | { kind: "budget_exceeded"; turn: number; budget: string }
  | { kind: "milestone_advanced"; turn: number; phase_id: string; capabilities_unlocked: string[] }
  | { kind: "milestone_blocked"; turn: number; phase_id: string; reason: string }
  | { kind: "checkpoint_taken"; turn: number; history_len: number }
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
  | { kind: "memory_written"; turn: number; memory_id: string; memory_kind: string; size_bytes: number }
  | { kind: "memory_queried"; turn: number; query_context: string; requested_k: number; requires_async_response: boolean }
  | { kind: "memory_validation_failed"; turn: number; memory_id: string; error: string }
  | { kind: "memory_retrieval_result"; selected_memory_ids: string[]; selection_rationale: string }
  | {
      kind: "workflow_node_completed"
      turn: number
      agent_id: string
      termination: string
    }
  | {
      kind: "workflow_nodes_submitted"
      turn: number
      /** Kernel-shape (snake_case) submitted node specs — persisted so resume can re-apply them. */
      nodes: Record<string, unknown>[]
      /** R3-1: graph base index the batch was appended at (from the kernel's
       *  WorkflowNodesSubmitted observation) — lets resume rebuild exact indices. */
      base_index?: number
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
      completed: string[]
      failed: string[]
      total_nodes: number
    }
  | { kind: "run_terminal"; reason: string; turns_used: number; total_tokens: number }
  | { kind: "summary_upgraded"; compressed_seq: number; summary: string }
  // L1 (RunGroup): group-ledger events, appended under a group-anchor key (= the group id) so the
  // governance domain's cumulative budget + membership (lineage) persist and rebuild by fold-on-read.
  | { kind: "group_member_joined"; session_id: string; role?: string }
  | { kind: "group_budget_charged"; tokens: number; subagents: number }

export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>
}

export class InMemorySessionLog implements SessionLog {
  private store = new Map<string, Array<{ seq: number; event: SessionEvent }>>()

  async append(sessionId: string, event: SessionEvent): Promise<number> {
    if (!this.store.has(sessionId)) this.store.set(sessionId, [])
    const entries = this.store.get(sessionId)!
    const seq = entries.length
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
    const entries = this.store.get(sessionId)
    return entries ? entries.length - 1 : -1
  }
}

// Single-writer per session. Safe for concurrent appends within one instance.
// Cross-instance (multi-process) safety requires an external lock.
export class FileSessionLog implements SessionLog {
  // Lazy-initialized per-session counter. Avoids re-reading the file on every append.
  private seqCounters = new Map<string, number>()

  constructor(private dir: string) {}

  private path(sessionId: string): string {
    return join(this.dir, `${sessionId}.jsonl`)
  }

  private async nextSeq(sessionId: string): Promise<number> {
    if (!this.seqCounters.has(sessionId)) {
      const existing = await this.read(sessionId)
      this.seqCounters.set(sessionId, existing.length)
    }
    const seq = this.seqCounters.get(sessionId)!
    this.seqCounters.set(sessionId, seq + 1)
    return seq
  }

  async append(sessionId: string, event: SessionEvent): Promise<number> {
    await mkdir(this.dir, { recursive: true })
    const seq = await this.nextSeq(sessionId)
    await new Promise<void>((resolve, reject) => {
      const ws = createWriteStream(this.path(sessionId), { flags: "a" })
      ws.write(JSON.stringify({ seq, event }) + "\n", err => {
        if (err) reject(err)
        else resolve()
      })
      ws.end()
    })
    return seq
  }

  async read(sessionId: string, fromSeq = 0, primitiveFilter?: KernelPrimitive): Promise<Array<{ seq: number; event: SessionEvent }>> {
    const results: Array<{ seq: number; event: SessionEvent }> = []
    try {
      const rl = createInterface({
        input: createReadStream(this.path(sessionId)),
        crlfDelay: Infinity,
      })
      for await (const line of rl) {
        if (!line.trim()) continue
        const entry = JSON.parse(line) as { seq: number; event: SessionEvent }
        if (entry.seq >= fromSeq) {
          if (primitiveFilter && primitiveForKind(entry.event.kind) !== primitiveFilter) continue
          results.push(entry)
        }
      }
    } catch (err: unknown) {
      if ((err as { code?: string }).code !== "ENOENT") throw err
    }
    return results
  }

  async latestSeq(sessionId: string): Promise<number> {
    const entries = await this.read(sessionId)
    return entries.length - 1
  }
}
