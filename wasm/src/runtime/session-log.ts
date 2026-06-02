import type { ProviderReplay, ToolCall, ToolErrorKind } from "../types.js"
import type { KernelEventCategory } from "./kernel-event-log.js"

export type RollbackReason =
  | { kind: "fatal_tool_error"; tool_name: string; error: string }
  | { kind: "governance_denied"; tool_name: string; reason: string }
  | { kind: "provider_failure"; error: string }
  | { kind: "timeout" }
  | { kind: "user_interrupt" }
  | { kind: "malformed_replay"; reason: string }

export type SessionEvent =
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string }
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
      category?: KernelEventCategory
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
      category?: KernelEventCategory
      action?: "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
      summary?: string
      tier_hint?: string
      message_count?: number
    }
  | { kind: "page_in"; turn: number; category?: KernelEventCategory; entry_count: number }
  | { kind: "rollbacked"; turn: number; category?: KernelEventCategory; checkpoint_history_len: number; reason?: RollbackReason }
  | { kind: "capability_changed"; turn: number; category?: KernelEventCategory; added: string[]; removed: string[]; change_kind?: string; capability_id?: string; version?: string; mounted_by?: string; mount_reason?: string }
  | { kind: "context_renewed"; turn: number; category?: KernelEventCategory; sprint: number; handoff_ref: string }
  | { kind: "suspended"; turn: number; category?: KernelEventCategory; reason: string; pending_calls?: string[] }
  | { kind: "resumed"; turn: number; category?: KernelEventCategory; approved?: string[]; denied?: string[] }
  | { kind: "tool_gated"; turn: number; category?: KernelEventCategory; call_id: string; tool: string; reason: string }
  | { kind: "signal_disposed"; turn: number; category?: KernelEventCategory; signal_id: string; disposition: string; queue_depth: number }
  | { kind: "budget_exceeded"; turn: number; category?: KernelEventCategory; budget: string }
  | { kind: "milestone_advanced"; turn: number; category?: KernelEventCategory; phase_id: string; capabilities_unlocked: string[] }
  | { kind: "milestone_blocked"; turn: number; category?: KernelEventCategory; phase_id: string; reason: string }
  | { kind: "milestone_evidence"; turn: number; category?: KernelEventCategory; phase_id: string; evidence: string[] }
  | { kind: "checkpoint_taken"; turn: number; category?: KernelEventCategory; history_len: number }
  | {
      kind: "agent_process_changed"
      turn: number
      category?: KernelEventCategory
      agent_id: string
      parent_session_id: string
      role: string
      isolation: string
      context_inheritance: string
      state?: string
      permitted_capability_ids: string[]
      result_termination?: string
    }
  | {
      kind: "agent_spawned"
      turn: number
      agent_id: string
      parent_session_id: string
      role: string
      isolation: string
      context_inheritance: string
      permitted_capability_ids: string[]
    }
  | { kind: "run_terminal"; reason: string; turns_used: number; total_tokens: number }
  | { kind: "summary_upgraded"; compressed_seq: number; summary: string }

export interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>
  read(sessionId: string, fromSeq?: number): Promise<Array<{ seq: number; event: SessionEvent }>>
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

  async read(sessionId: string, fromSeq = 0): Promise<Array<{ seq: number; event: SessionEvent }>> {
    return (this.store.get(sessionId) ?? []).filter(e => e.seq >= fromSeq)
  }

  async latestSeq(sessionId: string): Promise<number> {
    const entries = this.store.get(sessionId)
    return entries ? entries.length - 1 : -1
  }
}
