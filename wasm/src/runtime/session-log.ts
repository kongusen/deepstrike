import type { ProviderReplay, ToolCall } from "../types.js"

export type SessionEvent =
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string }
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; provider_replay?: ProviderReplay }
  | { kind: "tool_requested"; turn: number; calls: ToolCall[] }
  | { kind: "tool_completed"; turn: number; results: Array<{ call_id: string; output: string; is_error?: boolean; token_count?: number }> }
  | { kind: "compressed"; turn: number; archived_seq_range: [number, number] }
  | { kind: "run_terminal"; reason: string; turns_used: number; total_tokens: number }

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
