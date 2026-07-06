/**
 * The Research Brief Studio's domain tools, shared across the curriculum.
 *
 * These are LOCAL MOCKS over a small canned corpus — the *provider* is real, the *tools* are
 * deterministic stand-ins for a search index + document store, so the examples need no network
 * or API keys beyond the LLM. Later levels layer real mechanisms (memory, skills, governance,
 * workflow) ON TOP of these same two tools, so the domain stays constant while the framework
 * surface grows.
 */
import { tool } from "@deepstrike/sdk"
import type { RegisteredTool } from "@deepstrike/sdk"

export interface Source {
  id: string
  title: string
  url: string
  /** One-line snippet returned by `search`. */
  snippet: string
  /** Full text returned by `read_source`. */
  body: string
  tags: string[]
}

/** A small on-topic corpus (the framework's own problem space — a nice meta touch). */
export const CORPUS: Source[] = [
  {
    id: "src-cache",
    title: "Prompt caching keeps a long, byte-stable prefix",
    url: "https://studio.local/sources/prompt-caching",
    snippet: "A cache hit requires the prefix bytes to be identical turn to turn; volatile content belongs at the end.",
    body: "Providers cache on an exact byte-prefix. The agent runtime keeps system + knowledge frozen at the front and pushes the volatile state turn to the tail, so the cacheable region grows monotonically. Reordering or rewriting an early message busts every downstream cache entry.",
    tags: ["cache", "context", "performance"],
  },
  {
    id: "src-memory",
    title: "Agent memory is written through one governed gate",
    url: "https://studio.local/sources/agent-memory",
    snippet: "Every memory write passes a single syscall — validation, quota, and dedup come for free.",
    body: "Rather than letting any code path append to a store, memory writes route through one WriteMemory syscall. That gate applies schema validation, a rolling-window rate limit, jaccard-similarity dedup, and an advisory relevance score. Retrieval at run-start seeds the decaying history so the model sees prior facts on turn one.",
    tags: ["memory", "governance"],
  },
  {
    id: "src-loop",
    title: "A loop agent is not a new engine",
    url: "https://studio.local/sources/loop-agent",
    snippet: "One round is one bounded run; continuity is the transcript; the only new decision is 'what next'.",
    body: "A self-pacing loop agent reuses the ordinary bounded run as its round. Continuity comes from replaying one stable session id, lifetime governance from a run group, and the sole new decision — continue / sleep / stop after a round — is a model-proposed, kernel-adjudicated pace verb. Silence means done.",
    tags: ["loop", "orchestration"],
  },
  {
    id: "src-workflow",
    title: "Dynamic workflows schedule sub-agents as a governed DAG",
    url: "https://studio.local/sources/dynamic-workflow",
    snippet: "Fan-out workers, a deterministic reduce, a quality gate — every node spawn passes the same gate.",
    body: "A workflow is a declarative DAG whose nodes lower to sub-agent run specs. Node kinds cover spawn, loop, classify, tournament, and a host-computed reduce. Every node spawn passes the one syscall gate (quota, quarantine, per-node caps), and a dependent node receives its dependencies' outputs — a DAG edge carries data, not just ordering.",
    tags: ["workflow", "orchestration", "governance"],
  },
  {
    id: "src-signals",
    title: "External events reach the agent as attention signals",
    url: "https://studio.local/sources/signals-reactive",
    snippet: "A webhook ingests a signal; it drains at the next turn boundary through the attention policy.",
    body: "A signal gateway ingests external events (webhooks, cron, completions). Each drains at a turn boundary and routes through the in-kernel attention policy, which decides whether to queue, soft-interrupt, or preempt. Recipient-addressed signals let one gateway serve many peer agents.",
    tags: ["signals", "reactive"],
  },
]

/** Naive keyword search over the corpus (title + snippet + tags). Returns compact stubs. */
export function searchTool(corpus: Source[] = CORPUS): RegisteredTool {
  return tool(
    "search",
    "Search the studio's source index for a query. Returns matching sources as {id, title, snippet, url}. Read a source's full text with read_source(id).",
    {
      type: "object",
      properties: { query: { type: "string", description: "Keywords to search for." } },
      required: ["query"],
    },
    (args) => {
      const q = String(args.query ?? "").toLowerCase()
      const terms = q.split(/\s+/).filter(Boolean)
      const hits = corpus
        .map((s) => {
          const hay = `${s.title} ${s.snippet} ${s.tags.join(" ")}`.toLowerCase()
          const score = terms.reduce((n, t) => n + (hay.includes(t) ? 1 : 0), 0)
          return { s, score }
        })
        .filter((h) => h.score > 0)
        .sort((a, b) => b.score - a.score)
        .slice(0, 5)
        .map((h) => ({ id: h.s.id, title: h.s.title, snippet: h.s.snippet, url: h.s.url }))
      return JSON.stringify(hits.length ? hits : { note: "no matching sources", query: q })
    },
  )
}

/** Fetch the full body of a source by id (the "open the document" step). */
export function readSourceTool(corpus: Source[] = CORPUS): RegisteredTool {
  return tool(
    "read_source",
    "Read the full text of one source by its id (from search results). Returns {id, title, url, body}.",
    {
      type: "object",
      properties: { id: { type: "string", description: "The source id, e.g. 'src-cache'." } },
      required: ["id"],
    },
    (args) => {
      const id = String(args.id ?? "")
      const src = corpus.find((s) => s.id === id)
      if (!src) return JSON.stringify({ error: `no source with id '${id}'`, known_ids: corpus.map((s) => s.id) })
      return JSON.stringify({ id: src.id, title: src.title, url: src.url, body: src.body })
    },
  )
}

/** The two studio tools every level starts from. */
export function studioTools(corpus: Source[] = CORPUS): RegisteredTool[] {
  return [searchTool(corpus), readSourceTool(corpus)]
}
