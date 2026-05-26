import { createWriteStream, createReadStream } from "node:fs"
import { mkdir } from "node:fs/promises"
import { join } from "node:path"
import { createInterface } from "node:readline"
import type { ProviderReplay, ToolCall } from "../types.js"

export type SessionEvent =
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string }
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; provider_replay?: ProviderReplay }
  | { kind: "tool_requested"; turn: number; calls: ToolCall[] }
  | { kind: "tool_completed"; turn: number; results: Array<{ call_id: string; output: string; is_error?: boolean; token_count?: number }> }
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

  async read(sessionId: string, fromSeq = 0): Promise<Array<{ seq: number; event: SessionEvent }>> {
    const results: Array<{ seq: number; event: SessionEvent }> = []
    try {
      const rl = createInterface({
        input: createReadStream(this.path(sessionId)),
        crlfDelay: Infinity,
      })
      for await (const line of rl) {
        if (!line.trim()) continue
        const entry = JSON.parse(line) as { seq: number; event: SessionEvent }
        if (entry.seq >= fromSeq) results.push(entry)
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
