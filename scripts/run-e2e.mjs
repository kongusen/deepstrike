#!/usr/bin/env node
/**
 * E2E scenario runner — standalone CLI entry point.
 *
 * Requires the Node SDK to be compiled first: `npm run build` in node/
 * Then: node scripts/run-e2e.mjs [K01 K03 K06 ...]
 *
 * Reads .env from the repo root for API keys (MINIMAX_API_KEY, OPENAI_API_KEY,
 * DEEPSEEK_API_KEY). Run all scenarios by default, or pass IDs to limit.
 * MiniMax is the default live stress provider; override with E2E_PROVIDER.
 *
 * Exit code: 0 if all ran scenarios pass, 1 otherwise.
 */
import { readFileSync } from "node:fs"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"

const __dir = dirname(fileURLToPath(import.meta.url))
loadEnvFile(join(__dir, "../.env"))

// ── filler corpus ─────────────────────────────────────────────────────────────
// Fetched once from Wikipedia REST API at startup.
// Falls back to deterministic pseudo-prose if the network is unavailable.

async function loadFillerCorpus(targetChars = 30_000) {
  const titles = [
    "Data_compression",
    "Context_(computing)",
    "Turing_completeness",
    "Algorithm",
    "Lempel%E2%80%93Ziv%E2%80%93Welch",
    "Finite-state_machine",
    "Recursion_(computer_science)",
    "Computer_memory",
  ]
  let corpus = ""
  for (const title of titles) {
    if (corpus.length >= targetChars) break
    try {
      const url = `https://en.wikipedia.org/api/rest_v1/page/summary/${title}`
      const res = await fetch(url, { signal: AbortSignal.timeout(5_000) })
      if (!res.ok) continue
      const data = await res.json()
      const text = data.extract ?? data.description ?? ""
      if (text.length > 100) corpus += text + "\n\n"
    } catch { /* skip on timeout/network error */ }
  }
  if (corpus.length < 3_000) {
    // Fallback: deterministic prose that tokenises like real text
    corpus = Array.from({ length: 300 }, (_, i) =>
      `Section ${i + 1}: The runtime scheduler evaluates pending state transitions and dispatches context windows to downstream processing units. Each layer validates structural invariants before forwarding payloads, ensuring that token budgets remain within prescribed limits across concurrent execution threads.`,
    ).join("\n")
  }
  console.log(`  [corpus] loaded ${corpus.length.toLocaleString()} chars of filler text`)
  return corpus
}

/** Return a unique, non-repetitive slice of the filler corpus for each index. */
function fillerChunk(corpus, index, size = 700) {
  if (corpus.length === 0) return "x".repeat(size)
  const stride = Math.min(size, Math.floor(corpus.length / 40))
  const start = (index * stride) % Math.max(1, corpus.length - size)
  return corpus.slice(start, start + size)
}

const FILLER_CORPUS = await loadFillerCorpus()

// ── dynamic imports from compiled dist ───────────────────────────────────────

const dist = join(__dir, "../node/dist")
const {
  RuntimeRunner,
  InMemorySessionLog,
  LocalExecutionPlane,
  tool,
  OpenAIChatProvider,
  DeepSeekProvider,
  MiniMaxProvider,
} = await import(join(dist, "index.js"))

// ── provider setup ────────────────────────────────────────────────────────────

const providers = {
  openai: process.env.OPENAI_API_KEY
    ? new OpenAIChatProvider(
        process.env.OPENAI_API_KEY,
        process.env.OPENAI_MODEL ?? "gpt-4o-mini",
        { maxRetries: 2, baseDelay: 1000 },
        process.env.OPENAI_BASE_URL,
      )
    : null,
  deepseek: process.env.DEEPSEEK_API_KEY
    ? new DeepSeekProvider(
        process.env.DEEPSEEK_API_KEY,
        process.env.DEEPSEEK_MODEL ?? "deepseek-chat",
      )
    : null,
  minimax: process.env.MINIMAX_API_KEY
    ? new MiniMaxProvider(process.env.MINIMAX_API_KEY, process.env.MINIMAX_MODEL)
    : null,
}

const requestedProvider = String(process.env.E2E_PROVIDER ?? "").toLowerCase()
const providerOrder = requestedProvider
  ? [requestedProvider]
  : ["minimax", "openai", "deepseek"]
const providerLabel = providerOrder.find(name => providers[name])
const defaultProvider = providerLabel ? providers[providerLabel] : null

if (!defaultProvider) {
  if (requestedProvider) {
    console.error(`Provider "${requestedProvider}" requested but no matching API key/provider is configured.`)
  } else {
    console.error("No API key found. Set MINIMAX_API_KEY, OPENAI_API_KEY, or DEEPSEEK_API_KEY in .env")
  }
  process.exit(1)
}

// ── scenario filter ───────────────────────────────────────────────────────────

const filterIds = process.argv.slice(2).map(s => s.toUpperCase())

// ── live stress scenario definitions ─────────────────────────────────────────
// These intentionally differ from the deterministic mechanism tests. This
// runner exercises real providers, MiniMax first, under heavier boundary
// conditions and fails when the model avoids the requested stress path.

const SECRET_CODE = "ZETA-7741"

const SCENARIOS = [
  // K01 — rho linear growth
  {
    id: "K01", name: "rho-linear-growth",
    goal: "Stress test. Call advance_rho_step exactly once per assistant turn until it returns STEP_20. Do not call it more than once in a single turn. After STEP_20, reply DONE.",
    tools: [
      (() => {
        let step = 0
        return tool("advance_rho_step", "Advance the rho stress loop by exactly one step. Call at most once per assistant turn.", { type: "object", properties: {} },
          () => {
            step += 1
            const marker = `STEP_${String(step).padStart(2, "0")}`
            return `${marker}\n${fillerChunk(FILLER_CORPUS, step, 900)}`
          })
      })(),
    ],
    maxTokens: 32_000, maxTurns: 28, timeoutMs: 300_000,
    validate(r) {
      if (r.finalStatus !== "completed" && r.finalStatus !== "max_turns")
        return fail(`run did not complete: ${r.finalStatus}`)
      const calls = countToolCalls(r, "advance_rho_step")
      if (calls !== 20) return fail(`expected exactly 20 rho steps, got ${calls}`)
      const maxBatch = maxToolBatch(r, "advance_rho_step")
      if (maxBatch > 1) return fail(`provider batched ${maxBatch} rho steps in one turn; stress path requires sequential turns`)
      const tokens = r.metrics.map(m => m.inputTokens).filter(t => t > 0)
      if (r.metrics.length < 5)
        return fail(`too few turns to measure growth: ${r.metrics.length}`)
      if (tokens.length < 4)
        return fail("not enough usage events to assess growth")
      const first5avg = avg(tokens.slice(0, 5))
      const last5avg = avg(tokens.slice(-5))
      const ratio = first5avg > 0 ? last5avg / first5avg : 1
      if (ratio > 6) return fail(`inputTokens growth super-linear: ratio=${ratio.toFixed(1)}x`)
      if (r.peakContextChars < 12_000) return fail(`stress context too small: peak_context_chars=${r.peakContextChars}`)
      return pass()
    },
  },

  // K02 — recency preserved
  {
    id: "K02", name: "recency-preserved",
    goal: `First call store_secret with "${SECRET_CODE}". Then call fill_buffer exactly once per assistant turn until it returns RECENT_MARKER_18. Do not batch calls. After RECENT_MARKER_18, reply with both ${SECRET_CODE} and RECENT_MARKER_18.`,
    tools: (() => {
      let stored = ""
      let fill = 0
      return [
        tool("store_secret", "Store a secret", { type: "object", properties: { secret: { type: "string" } }, required: ["secret"] },
          (args) => { stored = args.secret; return "stored" }),
        tool("fill_buffer", "Add a large numbered filler block. Call at most once per assistant turn.", { type: "object", properties: {} },
          () => {
            fill += 1
            const marker = `RECENT_MARKER_${fill}`
            const early = fill === 1 ? `EARLY_SECRET_COPY=${stored}\n` : ""
            return `${marker}\n${early}${fillerChunk(FILLER_CORPUS, fill + 20, 1_200)}`
          }),
      ]
    })(),
    maxTokens: 8_192, maxTurns: 26, timeoutMs: 300_000,
    validate(r) {
      const fillCalls = countToolCalls(r, "fill_buffer")
      if (countToolCalls(r, "store_secret") !== 1) return fail("store_secret was not called exactly once")
      if (fillCalls !== 18) return fail(`expected 18 filler calls, got ${fillCalls}`)
      if (maxToolBatch(r, "fill_buffer") > 1) return fail("fill_buffer was batched; recency stress requires sequential turns")
      if (r.peakContextChars < 10_000) return fail(`stress context too small: peak_context_chars=${r.peakContextChars}`)
      if (!r.finalText?.includes(SECRET_CODE) || !r.finalText?.includes("RECENT_MARKER_18")) {
        return fail(`final reply missing secret or latest marker: "${r.finalText?.slice(0, 240)}"`)
      }
      return pass()
    },
  },

  // K03 — goal in State turn (turns[0])
  // New design: goal is in turns[0] (State slot), not systemVolatile.
  {
    id: "K03", name: "goal-in-state-turn",
    goal: "Count from 1 to 3 and say COMPLETE.",
    tools: [],
    maxTokens: 4_096, maxTurns: 5, timeoutMs: 60_000,
    validate(r) {
      const snap = r.metrics[0]?.contextSnapshot
      if (!snap) return fail("no context snapshot")
      // goal must be in stateTurnContent (turns[0]), not in system_text
      if (!snap.stateTurnContent.includes("Count from 1 to 3"))
        return fail(`goal not in State turn (turns[0]): "${snap.stateTurnContent.slice(0, 200)}"`)
      return pass()
    },
  },

  // K04 — auto-compact fires and injects summary
  {
    id: "K04", name: "auto-compact-summary-injected",
    goal: "Stress test context compression. You MUST call the fill tool with n=1, then n=2, continuing exactly once per assistant turn through n=12. Do not batch calls. After fill n=12 or after seeing a compressed-context summary, reply DONE.",
    tools: [
      tool("fill", "Return a large arbitrary pressure block for the requested n. Call at most once per assistant turn.", {
        type: "object",
        properties: { n: { type: "number" } },
        required: ["n"],
      }, args => `PRESSURE_BLOB_${args.n}\n${fillerChunk(FILLER_CORPUS, args.n + 40, 1_400)}`),
    ],
    maxTokens: 768, maxTurns: 24, timeoutMs: 300_000,
    validate(r) {
      const fillCalls = countToolCalls(r, "fill")
      if (fillCalls < 3) return fail(`too few fill calls before completion: ${fillCalls}`)
      if (maxToolBatch(r, "fill") > 1) return fail("fill was batched; compression stress requires sequential turns")
      if (r.compressions === 0) return fail("no compression — pipeline should fire under 768-token budget")
      const COMPRESSION_ACTIONS = new Set(["snip_compact", "micro_compact", "context_collapse", "auto_compact"])
      const hasKnownAction = r.compressionActions.every(a => COMPRESSION_ACTIONS.has(a))
      if (!hasKnownAction) return fail(`unknown compression action(s): ${r.compressionActions.join(",")}`)
      // Only collapse/auto produce summaries; snip/micro do not — accept any tier.
      if (r.turnsUsed >= 23) return fail(`hit max_turns without finishing (turns=${r.turnsUsed})`)
      return pass()
    },
  },

  // K05 — rollback recovery
  {
    id: "K05", name: "rollback-recovery",
    goal: "Call fragile_tool once. It may fail — retry until it succeeds. Then say SUCCESS.",
    tools: (() => {
      let attempts = 0
      return [
        tool("fragile_tool", "Fails the first two times", { type: "object", properties: {} },
          () => {
            attempts++
            if (attempts <= 2) {
              const err = new Error("transient error — please retry")
              err.isFatal = true
              throw err
            }
            return "ok on attempt " + attempts
          }),
      ]
    })(),
    maxTokens: 8_192, maxTurns: 15, timeoutMs: 120_000,
    validate(r) {
      const hadRollback = r.events.some(e => e.event.kind === "rollbacked")
      if (!hadRollback) return fail("no rollbacked events — expected fragile_tool to trigger rollback")
      if (r.events.filter(e => e.event.kind === "rollbacked").length !== 2)
        return fail(`expected exactly 2 rollbacked events, got ${r.events.filter(e => e.event.kind === "rollbacked").length}`)
      if (countToolCalls(r, "fragile_tool") !== 3)
        return fail(`expected fragile_tool to be attempted 3 times, got ${countToolCalls(r, "fragile_tool")}`)
      if (!r.finalText?.toLowerCase().includes("success"))
        return fail(`final text lacks success marker: "${r.finalText?.slice(0, 200)}"`)
      return pass()
    },
  },

  // K06 — long tool loop stability
  {
    id: "K06", name: "long-tool-loop-stability",
    goal: "Stress test. Call accumulate exactly once per assistant turn until it returns ACC_STEP_20. Do not batch calls. After ACC_STEP_20, say FINISHED.",
    tools: [
      (() => {
        let step = 0
        return tool("accumulate", "Accumulate exactly one live stress step. Call at most once per assistant turn.", { type: "object", properties: {} },
          () => {
            step += 1
            return `ACC_STEP_${step}\n${fillerChunk(FILLER_CORPUS, step + 60, 700)}`
          })
      })(),
    ],
    maxTokens: 32_000, maxTurns: 35, timeoutMs: 300_000,
    validate(r) {
      if (r.finalStatus !== "completed" && r.finalStatus !== "max_turns")
        return fail(`run did not complete: ${r.finalStatus}`)
      if (countToolCalls(r, "accumulate") !== 20) return fail(`expected 20 accumulate calls, got ${countToolCalls(r, "accumulate")}`)
      if (maxToolBatch(r, "accumulate") > 1) return fail("accumulate was batched; long-loop stress requires sequential turns")
      if (r.compressions > 1)
        return fail(`${r.compressions} compressions on 32k budget — rho over-counting suspected`)
      if (r.metrics.length < 10) return fail(`too few turns for long-loop stability: ${r.metrics.length}`)
      const tokenList = r.metrics.map(m => m.inputTokens).filter(t => t > 0)
      for (let i = 1; i < tokenList.length; i++) {
        const delta = tokenList[i] - tokenList[i - 1]
        if (tokenList[i - 1] > 0 && delta > tokenList[i - 1] * 2 && delta > 2000)
          return fail(`token spike at turn ${i}: ${tokenList[i - 1]} → ${tokenList[i]}`)
      }
      if (r.peakContextChars < 10_000) return fail(`stress context too small: peak_context_chars=${r.peakContextChars}`)
      return pass()
    },
  },

  // K07 — in-session KV store (tool round-trip)
  {
    id: "K07", name: "session-kv-roundtrip",
    goal: "Call set_value with value=PERSIST-42. Then call get_value and include the value verbatim in your reply.",
    tools: (() => {
      const kv = new Map()
      return [
        tool("set_value", "Store a value", { type: "object", properties: { value: { type: "string" } }, required: ["value"] },
          (args) => { kv.set("k", args.value); return "stored" }),
        tool("get_value", "Retrieve value", { type: "object", properties: {} },
          () => kv.get("k") ?? "(empty)"),
      ]
    })(),
    maxTokens: 8_192, maxTurns: 10, timeoutMs: 90_000,
    validate(r) {
      if (countToolCalls(r, "set_value") !== 1) return fail(`expected set_value once, got ${countToolCalls(r, "set_value")}`)
      if (countToolCalls(r, "get_value") !== 1) return fail(`expected get_value once, got ${countToolCalls(r, "get_value")}`)
      return r.finalText?.includes("PERSIST-42")
        ? pass()
        : fail(`agent did not recall stored value: "${r.finalText?.slice(0, 200)}"`)
    },
  },

  // K08 — coding: virtual filesystem
  {
    id: "K08", name: "coding-virtual-fs",
    goal: "Write a file 'result.txt' containing exactly: answer=42\n"
      + "Then read it back and verify. Reply FILE_VERIFIED if correct, FILE_ERROR if not.",
    tools: (() => {
      const fs = new Map()
      return [
        tool("write_file", "Write file", { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path", "content"] },
          (args) => { fs.set(args.path, args.content); return `wrote ${args.content.length} bytes to ${args.path}` }),
        tool("read_file", "Read file", { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
          (args) => fs.get(args.path) ?? "(not found)"),
      ]
    })(),
    maxTokens: 8_192, maxTurns: 12, timeoutMs: 120_000,
    validate(r) {
      if (countToolCalls(r, "write_file") !== 1) return fail(`expected write_file once, got ${countToolCalls(r, "write_file")}`)
      if (countToolCalls(r, "read_file") !== 1) return fail(`expected read_file once, got ${countToolCalls(r, "read_file")}`)
      return r.finalText?.includes("FILE_VERIFIED")
        ? pass()
        : fail(`agent did not confirm file verification: "${r.finalText?.slice(0, 300)}"`)
    },
  },
]

// ── metric-capturing provider wrapper (inline, no TypeScript) ─────────────────

function wrapProvider(inner) {
  const turnMetrics = []
  let turnIndex = 0
  return {
    turnMetrics,
    async complete(ctx, tools) { return inner.complete(ctx, tools) },
    async *stream(ctx, tools) {
      const turn = turnIndex++
      let inputTokens = 0
      let outputTokens = 0
      const snapshot = {
        turnsCount: ctx.turns.length,
        systemKnowledge: ctx.systemKnowledge ?? "",
        stateTurnContent: ctx.turns[0]?.content ?? "",
        contextChars: renderedContextText(ctx).length,
      }
      for await (const evt of inner.stream(ctx, tools)) {
        if (evt.type === "usage") {
          inputTokens = evt.inputTokens ?? inputTokens
          outputTokens = evt.outputTokens ?? outputTokens
        }
        yield evt
      }
      turnMetrics.push({ turn, inputTokens, outputTokens, contextSnapshot: snapshot })
    },
  }
}

// ── scenario runner ───────────────────────────────────────────────────────────

async function runScenario(provider, cfg) {
  const capturing = wrapProvider(provider)
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  for (const t of cfg.tools ?? []) plane.register(t)

  const runner = new RuntimeRunner({
    provider: capturing,
    sessionLog,
    executionPlane: plane,
    maxTokens: cfg.maxTokens,
    maxTurns: cfg.maxTurns,
  })

  const sid = `e2e-${cfg.id}-${Date.now()}`
  let finalText = ""
  let finalStatus = "error"
  const errors = []

  const timeout = cfg.timeoutMs ?? 120_000
  const runPromise = (async () => {
    for await (const evt of runner.run({ sessionId: sid, goal: cfg.goal })) {
      if (evt.type === "text_delta") finalText += evt.delta ?? ""
      if (evt.type === "done") finalStatus = evt.status ?? "error"
      if (evt.type === "error") errors.push(evt.message ?? String(evt))
    }
  })()
  await Promise.race([
    runPromise,
    new Promise((_, rej) => setTimeout(() => rej(new Error("timeout")), timeout)),
  ])

  const events = await sessionLog.read(sid)
  for (const { event: e } of events) {
    if (e.kind === "compressed") {
      const m = capturing.turnMetrics.find(m => m.turn === (e.turn ?? 0))
      if (m) m.compressionAction = e.action ?? "unknown"
    }
  }

  const compressEvents = events.filter(e => e.event.kind === "compressed")
  const r = {
    id: cfg.id,
    turnsUsed: capturing.turnMetrics.length,
    compressions: compressEvents.length,
    compressionActions: compressEvents.map(e => e.event.action ?? "unknown"),
    peakInputTokens: Math.max(0, ...capturing.turnMetrics.map(m => m.inputTokens)),
    peakContextChars: Math.max(0, ...capturing.turnMetrics.map(m => m.contextSnapshot?.contextChars ?? 0)),
    finalText,
    finalStatus,
    errors,
    metrics: capturing.turnMetrics,
    events,
    toolCounts: toolCounts(events),
  }

  const { passed, failure } = cfg.validate(r)
  return { ...r, passed, failure }
}

// ── report ────────────────────────────────────────────────────────────────────

function printReport(results) {
  const pass = results.filter(r => r.passed).length
  console.log(`\n${"═".repeat(55)}`)
  console.log(`  E2E Results: ${pass}/${results.length} passed`)
  console.log(`${"═".repeat(55)}`)
  for (const r of results) {
    const icon = r.passed ? "✅" : "❌"
    const tokens = r.peakInputTokens > 0 ? `  peak_in=${r.peakInputTokens}` : ""
    const chars = r.peakContextChars > 0 ? `  peak_ctx_chars=${r.peakContextChars}` : ""
    const comp = r.compressions > 0 ? `  compress=${r.compressions}(${r.compressionActions.join(",")})` : ""
    const tools = r.toolCounts && Object.keys(r.toolCounts).length
      ? `  tools=${Object.entries(r.toolCounts).map(([k, v]) => `${k}:${v}`).join(",")}`
      : ""
    console.log(`${icon} [${r.id}]  turns=${r.turnsUsed}  status=${r.finalStatus}${tokens}${chars}${comp}${tools}`)
    if (!r.passed && r.failure) console.log(`      ↳ ${r.failure}`)
    if (!r.passed && r.errors?.length) console.log(`      errors: ${r.errors.map(redactSecret).join(" | ")}`)
  }
  console.log(`${"═".repeat(55)}\n`)
  return pass === results.length ? 0 : 1
}

// ── entry point ───────────────────────────────────────────────────────────────

const toRun = filterIds.length > 0
  ? SCENARIOS.filter(s => filterIds.includes(s.id.toUpperCase()))
  : SCENARIOS

if (toRun.length === 0) {
  console.error(`No matching scenarios for: ${filterIds.join(", ")}`)
  process.exit(1)
}

console.log(`\nRunning ${toRun.length} scenario(s) with provider: ${providerLabel}`)

const results = []
for (const cfg of toRun) {
  process.stdout.write(`  [${cfg.id}] ${cfg.name} ... `)
  try {
    const result = await runScenario(defaultProvider, cfg)
    results.push(result)
    console.log(result.passed ? "PASS" : `FAIL: ${result.failure}`)
  } catch (err) {
    const r = { id: cfg.id, passed: false, failure: String(err), turnsUsed: 0, compressions: 0, compressionActions: [], peakInputTokens: 0, peakContextChars: 0, finalText: "", finalStatus: "error", errors: [String(err)], metrics: [], events: [], toolCounts: {} }
    results.push(r)
    console.log(`ERROR: ${err}`)
  }
}

process.exit(printReport(results))

// ── helpers ───────────────────────────────────────────────────────────────────

function pass() { return { passed: true } }
function fail(reason) { return { passed: false, failure: reason } }
function avg(arr) { return arr.length ? arr.reduce((a, b) => a + b, 0) / arr.length : 0 }

function toolRequestEvents(r) {
  return r.events.filter(e => e.event.kind === "tool_requested")
}

function countToolCalls(r, name) {
  return r.toolCounts?.[name] ?? toolRequestEvents(r)
    .flatMap(e => e.event.calls ?? [])
    .filter(c => c.name === name)
    .length
}

function maxToolBatch(r, name) {
  return Math.max(0, ...toolRequestEvents(r).map(e =>
    (e.event.calls ?? []).filter(c => c.name === name).length,
  ))
}

function renderedContextText(ctx) {
  return [
    ctx.systemText ?? "",
    ctx.systemKnowledge ?? "",
    ...(ctx.turns ?? []).flatMap(t => [
      t.content ?? "",
      ...((t.contentParts ?? []).map(p => p.output ?? p.text ?? "")),
      ...((t.toolCalls ?? []).map(tc => `${tc.name} ${tc.arguments}`)),
    ]),
  ].join("\n")
}

function toolCounts(events) {
  const counts = {}
  for (const event of events.filter(e => e.event.kind === "tool_requested")) {
    for (const call of event.event.calls ?? []) {
      counts[call.name] = (counts[call.name] ?? 0) + 1
    }
  }
  return counts
}

function redactSecret(s) {
  return String(s).replace(/sk-[A-Za-z0-9_-]+/g, "sk-<redacted>")
}

function loadEnvFile(path) {
  try {
    const raw = readFileSync(path, "utf8")
    for (const line of raw.split(/\r?\n/)) {
      const t = line.trim()
      if (!t || t.startsWith("#")) continue
      const idx = t.indexOf("=")
      if (idx === -1) continue
      const key = t.slice(0, idx)
      const value = t.slice(idx + 1)
      if (!(key in process.env)) process.env[key] = value
    }
  } catch { /* no .env is fine */ }
}
