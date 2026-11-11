// Plan task 9.4 · offline consolidation is an ordinary run, not a kernel mechanism.
//
// Between sessions, a host may ask a model to reconcile its long-term memory against what actually
// happened. That is just an agent run: the model reads memory with `memory`, searches the session
// transcripts with `transcript_search`, and proposes corrected or missing records with
// `propose_memory`, which the kernel admits like any other proposal. When it runs and what it
// costs is the host's decision; the kernel does not know consolidation exists.

import { RuntimeRunner, collectText, type RuntimeOptions } from "../runtime/runner.js"
import { LocalExecutionPlane } from "../runtime/execution-plane.js"
import type { SessionLog, SessionEvent } from "../runtime/session-log.js"
import { tool, type RegisteredTool } from "../tools/index.js"

export const TRANSCRIPT_SEARCH_TOOL_NAME = "transcript_search"

/** One searchable line of a session transcript. */
export interface TranscriptLine {
  sessionId: string
  seq: number
  turn: number
  /** Where the text came from: the goal, the model's text, or a tool result (with its call). */
  source: string
  text: string
}

const SNIPPET_CHARS = 240

function text(value: unknown): string {
  if (typeof value === "string") return value
  if (value === undefined || value === null) return ""
  return JSON.stringify(value)
}

/** The searchable lines of one session's events: the goal, the model's text and its calls, and
 *  every tool result labeled with the call that produced it. */
export function transcriptLines(sessionId: string, entries: Array<{ seq: number; event: SessionEvent }>): TranscriptLine[] {
  const calls = new Map<string, string>()
  const lines: TranscriptLine[] = []
  for (const { seq, event } of entries) {
    const record = event as unknown as Record<string, unknown>
    const turn = typeof record.turn === "number" ? record.turn : 0
    switch (event.kind) {
      case "run_started":
        lines.push({ sessionId, seq, turn, source: "goal", text: text(record.goal) })
        break
      case "llm_completed": {
        const toolCalls = Array.isArray(record.tool_calls) ? record.tool_calls as Array<Record<string, unknown>> : []
        for (const call of toolCalls) calls.set(text(call.id), `${text(call.name)} ${text(call.arguments)}`)
        const said = text(record.content)
        if (said.trim()) lines.push({ sessionId, seq, turn, source: "model", text: said })
        break
      }
      case "tool_completed": {
        const results = Array.isArray(record.results) ? record.results as Array<Record<string, unknown>> : []
        for (const result of results) {
          const callId = text(result.call_id)
          lines.push({ sessionId, seq, turn, source: `${callId} ${calls.get(callId) ?? "tool"}`, text: text(result.output) })
        }
        break
      }
      default:
        break
    }
  }
  return lines
}

function terms(query: string): string[] {
  return [...new Set(query.toLowerCase().split(/[^\p{L}\p{N}_./-]+/u).filter(term => term.length > 1))]
}

/** Lines ranked by how many query terms they contain (ties: earlier first), each cut to a snippet
 *  around its first hit. Deterministic. */
export function searchTranscripts(lines: TranscriptLine[], query: string, limit = 8): TranscriptLine[] {
  const wanted = terms(query)
  if (wanted.length === 0) return []
  const scored = lines
    .map((line, index) => {
      const folded = line.text.toLowerCase()
      const hits = wanted.filter(term => folded.includes(term) || line.source.toLowerCase().includes(term))
      return { line, index, score: hits.length, first: hits.length ? Math.max(0, folded.indexOf(hits[0])) : 0 }
    })
    .filter(entry => entry.score > 0)
    .sort((a, b) => b.score - a.score || a.index - b.index)
    .slice(0, Math.max(1, limit))
  return scored.map(({ line, first }) => {
    const start = Math.max(0, first - SNIPPET_CHARS / 2)
    const snippet = line.text.slice(start, start + SNIPPET_CHARS)
    return { ...line, text: `${start > 0 ? "…" : ""}${snippet}${start + SNIPPET_CHARS < line.text.length ? "…" : ""}` }
  })
}

export interface TranscriptSearchOptions {
  sessionLog: SessionLog
  /** The sessions the tool may search. */
  sessionIds: string[] | (() => Promise<string[]> | string[])
  /** Results per call at most (default 8). */
  maxResults?: number
}

/** A host tool that searches session transcripts by keyword. Each result names its session, turn
 *  and source (the goal, the model, or `call_id tool arguments`), so a model can cite it. */
export function transcriptSearchTool(options: TranscriptSearchOptions): RegisteredTool {
  const cap = options.maxResults ?? 8
  return tool(
    TRANSCRIPT_SEARCH_TOOL_NAME,
    "Search the transcripts of earlier sessions: what was asked, what the model said, and what each \
tool returned. Use it to check a memory against what actually happened.",
    {
      type: "object",
      properties: {
        query: { type: "string", description: "Words to look for." },
        session_id: { type: "string", description: "Search only this session." },
        limit: { type: "integer", description: `Results to return (at most ${cap}).` },
      },
      required: ["query"],
    },
    async args => {
      const ids = typeof options.sessionIds === "function" ? await options.sessionIds() : options.sessionIds
      const only = typeof args.session_id === "string" && args.session_id ? [args.session_id] : ids
      const lines: TranscriptLine[] = []
      for (const sessionId of only) {
        if (!ids.includes(sessionId)) continue
        lines.push(...transcriptLines(sessionId, await options.sessionLog.read(sessionId)))
      }
      const limit = Math.min(cap, typeof args.limit === "number" && args.limit > 0 ? Math.floor(args.limit) : cap)
      const found = searchTranscripts(lines, String(args.query ?? ""), limit)
      if (found.length === 0) return "no matching lines"
      return found.map(line => `[${line.sessionId} · turn ${line.turn} · ${line.source}] ${line.text}`).join("\n")
    },
  )
}

/** The default consolidation goal: directions, not prohibitions (plan task 9.4). */
export const DEFAULT_CONSOLIDATION_GOAL = "Reconcile long-term memory with what actually happened in \
the earlier sessions. Read the memory records that matter (memory), and check each against the \
transcripts (transcript_search). Where the transcripts contradict a record, propose a corrected \
record with the same name; where they show a rule or lesson memory lacks, propose it; finish by \
proposing one index record that says which records cover what. Cite the session and turn a record \
rests on in its content. Reply with a short list of what you proposed."

export interface ConsolidateMemoryOptions
  extends Omit<RuntimeOptions, "executionPlane" | "baselineToolIds" | "sessionLog"> {
  /** The log the transcripts are read from; the consolidation run is recorded there too. */
  sessionLog: SessionLog
  /** The sessions to reconcile against. */
  sessionIds: string[]
  /** The run's own session id (default `consolidate-<n sessions>`). */
  sessionId?: string
  goal?: string
  maxResults?: number
}

/** Run one consolidation: an ordinary agent run with `transcript_search` added to its tools. The
 *  memory options (`memoryStore`, `memoryCandidateSink`, `memoryRuntimePolicy`, `agentId`,
 *  `memoryScope`) are the host's, unchanged: proposals go through the candidate ABI as usual. */
export async function consolidateMemory(options: ConsolidateMemoryOptions): Promise<{ sessionId: string; text: string }> {
  const { sessionIds, sessionId, goal, maxResults, ...runtime } = options
  const plane = new LocalExecutionPlane()
  plane.register(transcriptSearchTool({ sessionLog: options.sessionLog, sessionIds, maxResults }))
  const runner = new RuntimeRunner({
    ...runtime,
    executionPlane: plane,
    baselineToolIds: [TRANSCRIPT_SEARCH_TOOL_NAME],
  })
  const id = sessionId ?? `consolidate-${sessionIds.length}`
  const text = await collectText(runner.run({ sessionId: id, goal: goal ?? DEFAULT_CONSOLIDATION_GOAL }))
  return { sessionId: id, text }
}
