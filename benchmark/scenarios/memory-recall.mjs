/**
 * Scenario: memory-recall.
 *
 * Measures whether a pre-populated long-term memory shortcuts the agent's investigation. Both
 * variants share the same task — "diagnose last week's outage" — but the `memory-preloaded`
 * variant ships a `DreamStore` with the actual root cause already stored. We expect the agent to
 * surface the memory entry, skip the multi-tool drill-down, and finish in fewer turns at lower
 * cost; the unloaded variant must investigate via logs / db queries.
 *
 * The scenario carries its own `InMemoryDreamStore` (cloned from the SDK's tests/MockDreamStore
 * helper) — no new SDK export, no 4-SDK parity surface.
 *
 * Variants:
 *   - `memory-empty`    (baseline) — `dreamStore` is an empty in-memory store.
 *   - `memory-preloaded`           — same store with a single highly relevant memory entry
 *                                    pre-seeded. The kernel's memory subsystem surfaces it on
 *                                    the first `memory_query`.
 *
 * mechanismHook reports: tool-call breakdown + `memoryUsed` (was the pre-seeded fact in fact
 * surfaced this session) + `turnsToDone`.
 */

import { loadSdk } from "../utils/sdk.mjs"

// ── local in-memory DreamStore (matches the DreamStore protocol in node SDK) ───────────────
class InMemoryDreamStore {
  constructor(initialMemories = []) {
    /** @type {Map<string, any[]>} */
    this.sessions = new Map()
    /** @type {Map<string, Array<{ text: string, score: number, metadata: any }>>} */
    this.memories = new Map()
    this._initial = initialMemories
  }
  async loadSessions(agentId) { return this.sessions.get(agentId) ?? [] }
  async loadMemories(agentId) {
    if (this.memories.has(agentId)) return this.memories.get(agentId)
    if (this._initial.length > 0) {
      this.memories.set(agentId, [...this._initial])
      return this.memories.get(agentId)
    }
    return []
  }
  async commit(agentId, result, existing) {
    const kept = existing.filter((_, i) => !result.toRemoveIndices.includes(i))
    this.memories.set(agentId, [...kept, ...result.toAdd])
  }
  async search(agentId, _query, topK = 5) {
    const all = await this.loadMemories(agentId)
    return all.slice(0, topK)
  }
  async saveSession(data) {
    const list = this.sessions.get(data.agentId) ?? []
    list.push(data)
    this.sessions.set(data.agentId, list)
  }
}

// ── pre-seeded memory (only the `memory-preloaded` variant gets this) ─────────────────────
const PRELOADED = [
  {
    text:
      "Last week's payment-service outage (PROJ-2731) was a db connection-pool exhaustion. " +
      "The orders-service started spawning a worker per inbound request after release 2026.04.08, " +
      "the pool capped at 50, every additional checkout queued on a 30s timeout. Mitigation: " +
      "raise pool.max to 200 in config/db.yml AND switch orders-service to bounded worker pool.",
    score: 0.95,
    metadata: { kind: "project", session_id: "incident-2731" },
  },
]

const AGENT_ID = "memory-bench-agent"

// ── task ──────────────────────────────────────────────────────────────────
const TASKS = [
  {
    id: "diagnose-outage",
    goal:
      "There was a payment-service outage last week. Find the root cause and propose a fix. " +
      "Report (1) the root cause, (2) the exact change needed, and (3) the file path to edit. " +
      "Be concise — finish in plain text once you can answer all three.",
    criteria: [
      "the root cause is identified as db connection pool exhaustion (or equivalent)",
      "the proposed fix touches the connection-pool capacity in config/db.yml",
      "the agent finishes with a plain-text answer rather than burning turns on irrelevant tools",
    ],
  },
]

const SYSTEM = [
  "You are a senior SRE diagnosing a past incident. Use the tools sparingly.",
  "If your memory already gives you the answer, do NOT re-investigate — answer directly.",
  "Call exactly ONE tool per assistant turn. Finish with a plain-text summary covering root cause, change, and path.",
].join("\n")

// ── tool factory (read-only investigation surface) ────────────────────────
let _sdk
async function getSdk() { if (!_sdk) _sdk = await loadSdk(); return _sdk }

/** @param {string} _sid */
async function mkTools(_sid) {
  const sdk = await getSdk()
  const { tool } = sdk
  const j = o => JSON.stringify(o)
  const strSchema = (k) => ({ type: "object", properties: { [k]: { type: "string" } }, required: [k] })

  return [
    tool("read_logs", "Read application logs filtered by service/time.", { type: "object", properties: { service: { type: "string" }, since: { type: "string" } } },
      async a => `[2026-04-08 11:23] ${a.service ?? "?"}: pool=50 in_use=50 waiting=12  timeout=30s\n[2026-04-08 11:24] ${a.service ?? "?"}: queue length growing`),
    tool("query_db", "Run a read-only db query.", strSchema("sql"),
      async a => j({ sql: a.sql, rows: [{ service: "payment", pool_max: 50, current_in_use: 50 }] })),
    tool("read_file", "Read a source/config file.", strSchema("path"),
      async a => {
        if ((a.path ?? "").includes("db.yml")) return "pool:\n  max: 50\n  acquire_timeout_ms: 30000\n"
        return `// ${a.path}\n(no notable content)\n`
      }),
    tool("list_dir", "List files under a directory.", strSchema("path"),
      async a => j({ path: a.path, entries: ["config/db.yml", "src/orders.js", "src/payment.js"] })),
    tool("grep_files", "Grep a regex across files.", { type: "object", properties: { pattern: { type: "string" }, path: { type: "string" } }, required: ["pattern"] },
      async a => j({ pattern: a.pattern, hits: [{ file: "config/db.yml", line: 4 }] })),
    tool("git_log", "Recent commits on a path.", { type: "object", properties: { path: { type: "string" } } },
      async () => "abc123 ship new orders-service worker model (2026-04-08)\ndef456 cap pool.max at 50 (2025-11)\n"),
  ]
}

// ── mechanism hook ─────────────────────────────────────────────────────────
/** @param {{ events: any[], turnMetrics: any[], streamToolCalls: Array<{name: string, arguments: any}> }} args */
function mechanismHook({ events, streamToolCalls }) {
  /** @type {Record<string, number>} */
  const exec = {}
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind !== "tool_requested") continue
    for (const c of ev.calls ?? []) exec[c.name] = (exec[c.name] ?? 0) + 1
  }

  // Did the agent surface the pre-seeded memory? Cheap heuristic: scan the assistant text accumulated
  // across the session for the unique PROJ ticket id only present in the preloaded entry.
  let memoryUsed = 0
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind === "llm_completed" && typeof ev.content === "string" && ev.content.includes("PROJ-2731")) {
      memoryUsed = 1
      break
    }
  }

  // Total tool ATTEMPTS (from stream) — also exposes whether memory short-circuited the loop.
  const attempts = streamToolCalls?.length ?? 0

  return {
    toolsExecuted: Object.values(exec).reduce((s, n) => s + n, 0),
    toolAttempts: attempts,
    readLogsCount: exec.read_logs ?? 0,
    queryDbCount: exec.query_db ?? 0,
    readFileCount: exec.read_file ?? 0,
    gitLogCount: exec.git_log ?? 0,
    memoryUsed,
  }
}

// ── exported scenario ─────────────────────────────────────────────────────
/** @type {import("../core/scenario.mjs").BenchScenario} */
export const memoryRecallScenario = {
  id: "memory-recall",
  description: "Long-term memory A/B: pre-seeded fact vs. empty store on a diagnose-the-outage task",
  systemPrompt: SYSTEM,
  tasks: TASKS,
  mkTools,
  maxTurns: 12,
  maxTokens: 8192,
  timeoutMs: 240_000,
  mechanismHook,

  variantOrder: ["memory-empty", "memory-preloaded"],
  variants: {
    "memory-empty": {
      description: "no pre-seeded memory — agent must investigate via tools (baseline)",
      setup: () => ({
        runtimeOverlay: {
          dreamStore: new InMemoryDreamStore([]),
          agentId: AGENT_ID,
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
    "memory-preloaded": {
      description: "memory store carries the actual root cause — kernel surfaces it on memory_query",
      setup: () => ({
        runtimeOverlay: {
          dreamStore: new InMemoryDreamStore(PRELOADED),
          agentId: AGENT_ID,
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
  },
}
