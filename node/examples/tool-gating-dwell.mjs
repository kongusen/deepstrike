#!/usr/bin/env node
// P0-C tool-gating dwell experiment.
//
// Runs a handful of realistic dev tasks through a real LLM, collecting per-turn `onTurnMetrics`,
// and aggregates the numbers that decide whether P1-B (epoch skill gating) is worth its cache-bust
// cost: skill DWELL `D` (turns a skill stays loaded), tool exposure-vs-call ratio, and the
// prompt-cache split. Reads provider config from the repo .env (OPENAI_*).
//
// Usage:  node node/examples/tool-gating-dwell.mjs [--max-turns 12] [--tasks 4]

import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const exampleDir = path.dirname(fileURLToPath(import.meta.url))
const nodeRoot = path.resolve(exampleDir, "..")
const repoRoot = path.resolve(nodeRoot, "..")

loadEnvFile(path.join(repoRoot, ".env"))
loadEnvFile(path.join(nodeRoot, ".env"))

const argMaxTurns = intArg("--max-turns", 12)
const argTasks = intArg("--tasks", 4)
const argProvider = strArg("--provider", process.env.LLM_PROVIDER || "openai")

// Provider selection. DeepSeek's openai-chat endpoint reports prompt-cache hit tokens
// (→ cacheReadInputTokens), giving the cost-side data the OpenAI proxy hid.
const PROVIDERS = {
  openai: { provider: "openai", apiKey: process.env.OPENAI_API_KEY, model: process.env.OPENAI_MODEL || "gpt-4o-mini", baseURL: process.env.OPENAI_BASE_URL || "", endpoint: undefined },
  deepseek: { provider: "deepseek", apiKey: process.env.DEEPSEEK_API_KEY, model: process.env.DEEPSEEK_MODEL || "deepseek-chat", baseURL: "", endpoint: "deepseek.openai" },
}
const sel = PROVIDERS[argProvider]
if (!sel) { console.error(`Unknown --provider ${argProvider} (have: ${Object.keys(PROVIDERS).join(", ")})`); process.exit(1) }
const { provider, apiKey, model, baseURL, endpoint } = sel
if (!apiKey) { console.error(`Missing API key for ${provider} in .env`); process.exit(1) }

const sdk = await loadSdk()
const { FileSessionLog, LocalExecutionPlane, RuntimeRunner, createProvider, tool } = sdk

// ── skills (the thing we gate on) ─────────────────────────────────────────────
const gate = process.argv.includes("--gate") // P1-B: enable skill tool-gating
const STABLE_CORE = ["read_file", "list_dir", "run_bash"] // always exposed under gating
const skillDir = mkdtempSync(path.join(tmpdir(), "ds-gating-skills-"))
const SKILLS = {
  debugging: { desc: "Systematic root-cause debugging: reproduce, isolate, read the failing code, form a hypothesis, verify with tests, then fix.", tools: ["read_file", "search_code", "run_tests", "run_bash", "git_diff", "grep_files"] },
  "code-review": { desc: "Multi-axis review: correctness, security, performance, style. Read the diff and the touched files, then report findings.", tools: ["git_diff", "read_file", "git_log", "git_blame", "lint", "find_references"] },
  testing: { desc: "Test-driven development: write a failing test first, run it, implement the minimum to pass, run again, then refactor.", tools: ["run_tests", "read_file", "write_file", "coverage_report", "list_tests"] },
  refactoring: { desc: "Safe incremental refactoring: one behavior-preserving change at a time, run tests after each.", tools: ["read_file", "write_file", "search_code", "run_tests", "rename_symbol", "replace_in_file"] },
}
for (const [name, { desc, tools }] of Object.entries(SKILLS)) {
  // Gating engages only when a skill DECLARES allowed_tools (kernel narrows to it ∪ stable-core ∪
  // meta). So the ungated baseline (`--gate` absent) writes NO allowed_tools line.
  const allowedLine = gate ? `allowed_tools: ${tools.join(", ")}\n` : ""
  writeFileSync(
    path.join(skillDir, `${name}.md`),
    `---\nname: ${name}\ndescription: ${desc.split(":")[0]}.\nwhen_to_use: ${desc.split(":")[0]}.\n${allowedLine}effort: 2\n---\n\n# ${name}\n\n${desc}\n`,
  )
}

// ── tools (a realistic ~10-tool dev surface; mock outputs, no real I/O) ────────
let testRunsBySession = {}
function mkTools(sessionId) {
  const j = o => JSON.stringify(o)
  return [
    tool("read_file", "Read a source file.", strSchema("path"), async a => `// ${a.path}\nexport function handler(req){ /* ...40 lines... */ return req }\n`),
    tool("list_dir", "List files in a directory.", strSchema("path"), async a => j({ path: a.path, entries: ["index.js", "auth.js", "config.js", "loader.js", "auth.test.js"] })),
    tool("search_code", "Search the codebase for a string.", strSchema("query"), async a => j({ query: a.query, matches: [{ file: "src/auth.js", line: 42 }, { file: "src/config.js", line: 7 }] })),
    tool("run_tests", "Run the test suite (optionally a target).", strSchema("target", false), async a => {
      const n = (testRunsBySession[sessionId] = (testRunsBySession[sessionId] ?? 0) + 1)
      // Fail the first two runs (drives multi-turn investigation), then pass.
      return n < 3
        ? j({ passed: 11, failed: 1, failing: [{ test: "auth handles expired token", file: "tests/auth.test.js", line: 18, error: "expected 401, got 500" }] })
        : j({ passed: 12, failed: 0 })
    }),
    tool("run_bash", "Run a shell command.", strSchema("cmd"), async a => `$ ${a.cmd}\n(ok)`),
    tool("git_diff", "Show the working-tree diff.", { type: "object", properties: {} }, async () => "diff --git a/src/payment.js b/src/payment.js\n+ charge(amount) { /* no input validation */ }\n"),
    tool("git_log", "Show recent commits.", { type: "object", properties: {} }, async () => "abc123 fix auth\ndef456 add payment\n"),
    tool("lint", "Lint a file or the project.", strSchema("path", false), async a => j({ path: a.path ?? ".", warnings: 2, errors: 0 })),
    tool("format", "Format a file.", strSchema("path", false), async a => j({ path: a.path ?? ".", formatted: true })),
    tool("write_file", "Write content to a file.", { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path"] }, async a => j({ path: a.path, written: true })),
    // ── extra realistic dev surface, to model a production-scale toolset (~30) ──
    tool("git_status", "Show the working-tree status (staged, unstaged, untracked).", { type: "object", properties: {} }, async () => j({ staged: [], modified: ["src/loader.js"], untracked: [] })),
    tool("git_blame", "Show last-modifier per line for a file.", strSchema("path"), async a => j({ path: a.path, lines: [{ line: 1, commit: "abc123", author: "alice" }] })),
    tool("git_stash", "Stash or restore working-tree changes.", strSchema("action", false), async a => j({ action: a.action ?? "push", ok: true })),
    tool("create_branch", "Create and switch to a new git branch.", strSchema("name"), async a => j({ branch: a.name, created: true })),
    tool("type_check", "Run the static type checker over the project or a file.", strSchema("path", false), async a => j({ path: a.path ?? ".", errors: 0, warnings: 1 })),
    tool("build", "Build the project.", strSchema("target", false), async a => j({ target: a.target ?? "all", ok: true, durationMs: 4200 })),
    tool("install_deps", "Install project dependencies.", { type: "object", properties: {} }, async () => j({ installed: 142, ok: true })),
    tool("coverage_report", "Report test coverage for a path.", strSchema("path", false), async a => j({ path: a.path ?? ".", lines: 0.82, branches: 0.74 })),
    tool("list_tests", "List test files and cases matching a pattern.", strSchema("pattern", false), async a => j({ matches: ["tests/auth.test.js", "tests/config.test.js"] })),
    tool("find_references", "Find references to a symbol across the codebase.", strSchema("symbol"), async a => j({ symbol: a.symbol, refs: [{ file: "src/auth.js", line: 12 }, { file: "src/index.js", line: 3 }] })),
    tool("find_symbol", "Locate the definition of a symbol.", strSchema("symbol"), async a => j({ symbol: a.symbol, file: "src/config.js", line: 7 })),
    tool("rename_symbol", "Rename a symbol across the codebase.", { type: "object", properties: { from: { type: "string" }, to: { type: "string" } }, required: ["from", "to"] }, async a => j({ from: a.from, to: a.to, edits: 4 })),
    tool("replace_in_file", "Replace a string in a file.", { type: "object", properties: { path: { type: "string" }, find: { type: "string" }, replace: { type: "string" } }, required: ["path", "find", "replace"] }, async a => j({ path: a.path, replaced: 2 })),
    tool("read_logs", "Read recent application logs, optionally filtered.", strSchema("filter", false), async a => `[info] server started\n[warn] slow query in ${a.filter ?? "auth"}\n`),
    tool("query_db", "Run a read-only database query.", strSchema("sql"), async a => j({ sql: a.sql, rows: [{ id: 1 }, { id: 2 }] })),
    tool("http_request", "Make an HTTP request to a local service.", { type: "object", properties: { method: { type: "string" }, url: { type: "string" } }, required: ["url"] }, async a => j({ url: a.url, status: 200 })),
    tool("env_get", "Read an environment/config value by key.", strSchema("key"), async a => j({ key: a.key, value: "<redacted>" })),
    tool("file_stat", "Stat a file (size, mtime, mode).", strSchema("path"), async a => j({ path: a.path, size: 2480, mode: "0644" })),
    tool("grep_files", "Grep a regex across files under a path.", { type: "object", properties: { pattern: { type: "string" }, path: { type: "string" } }, required: ["pattern"] }, async a => j({ pattern: a.pattern, hits: 3 })),
    tool("diff_files", "Diff two files.", { type: "object", properties: { a: { type: "string" }, b: { type: "string" } }, required: ["a", "b"] }, async a => `--- ${a.a}\n+++ ${a.b}\n@@ -1 +1 @@\n`),
  ]
}

const SYSTEM = [
  "You are a senior engineer working in a code repo via tools.",
  "MANDATORY FIRST STEP: your very first action MUST be a call to the `skill` tool with the single",
  "most relevant skill name for the task (one of: debugging, code-review, testing, refactoring).",
  "Do NOT call any other tool until you have loaded a skill. After loading it, follow its guidance.",
  "Then do real multi-step work: investigate with tools across several turns before concluding. ~1 tool per turn.",
  "When the task is done, reply with a short plain-text summary (no tool call).",
].join("\n")

const TASKS = [
  { id: "debug", goal: "A test in tests/auth.test.js is failing. Investigate with the tools, find the root cause, and fix it. Run the tests until they pass." },
  { id: "review", goal: "Review the recent changes (use git_diff and read the touched files) for correctness, security, and style. Report findings." },
  { id: "test", goal: "Add unit tests for the parseConfig function in src/config.js using a test-driven approach. Run the tests." },
  { id: "refactor", goal: "Refactor src/loader.js to improve readability without changing behavior. Verify with the test suite after changes." },
].slice(0, argTasks)

console.log(JSON.stringify({ provider, model, baseURL: baseURL || "(default)", maxTurns: argMaxTurns, tasks: TASKS.map(t => t.id), gating: gate ? `on (stable-core: ${STABLE_CORE.join(",")})` : "off", skillDir }, null, 2))

const runRoot = path.join(exampleDir, ".gating-runs", `run-${stamp()}`)
mkdirSync(runRoot, { recursive: true })
const allMetrics = [] // { sessionId, taskId, turn, toolsExposed, toolsCalled, activeSkill, inputTokens, cacheReadTokens, cacheCreationTokens }

for (const task of TASKS) {
  const sessionId = `gating-${task.id}-${stamp()}`
  const llm = createProvider({ provider, model, apiKey, ...(baseURL ? { baseURL } : {}), ...(endpoint ? { endpoint } : {}), retry: { maxRetries: 2, baseDelay: 600 } })
  const executionPlane = new LocalExecutionPlane().register(...mkTools(sessionId))
  const runner = new RuntimeRunner({
    provider: llm,
    sessionLog: new FileSessionLog(path.join(runRoot, "sessions")),
    executionPlane,
    maxTokens: 60_000,
    maxTurns: argMaxTurns,
    agentId: "gating-dwell-agent",
    systemPrompt: SYSTEM,
    skillDir,
    ...(gate ? { stableCoreToolIds: STABLE_CORE } : {}),
    onTurnMetrics: m => allMetrics.push({ sessionId, taskId: task.id, ...m }),
  })

  process.stdout.write(`\n=== task: ${task.id} ===\n`)
  let toolCalls = 0
  try {
    const goal = `First, call the \`skill\` tool to load the most relevant skill. Then: ${task.goal}`
    for await (const ev of runner.run({ sessionId, goal, criteria: ["the task is addressed and summarized"] })) {
      if (ev.type === "tool_call") { toolCalls++; process.stdout.write(`  [tool] ${ev.name} ${shortArgs(ev.arguments)}\n`) }
      else if (ev.type === "error") process.stdout.write(`  [error] ${redact(String(ev.message ?? ""))}\n`)
      else if (ev.type === "done") process.stdout.write(`  [done] turns=${ev.turnsUsed ?? "?"} tokens=${ev.totalTokensUsed ?? "?"}\n`)
    }
  } catch (e) {
    process.stdout.write(`  [run-error] ${redact(String(e?.message ?? e))}\n`)
  }
  process.stdout.write(`  toolCalls(stream)=${toolCalls}\n`)
}

// ── aggregate ─────────────────────────────────────────────────────────────────
const report = aggregate(allMetrics)
const outPath = path.join(runRoot, "dwell-report.json")
writeFileSync(outPath, JSON.stringify({ config: { provider, model, maxTurns: argMaxTurns }, report, metrics: allMetrics }, null, 2))
console.log("\n\n================ DWELL REPORT ================")
console.log(JSON.stringify(report, null, 2))
console.log(`\nfull metrics + report → ${outPath}`)
console.log(verdict(report))

// ── helpers ───────────────────────────────────────────────────────────────────
function aggregate(metrics) {
  const bySession = {}
  for (const m of metrics) (bySession[m.sessionId] ??= []).push(m)

  const dwellSamples = [] // length of each maximal consecutive run of the same activeSkill
  let activations = 0     // number of distinct skill loads (transitions into a non-null skill)
  let turnsWithSkill = 0, turnsTotal = 0
  let exposedSum = 0, calledSum = 0, cacheRead = 0, cacheCreation = 0, inputSum = 0
  let maxCacheReadTurn = 0 // ≈ deepest bustable prefix (conservative P)
  const boundaryP = []     // cache prefix at each skill-load turn = the REAL bust cost (B only busts there)

  for (const sid of Object.keys(bySession)) {
    const turns = bySession[sid].sort((a, b) => a.turn - b.turn)
    let prev = undefined, runLen = 0
    for (const t of turns) {
      turnsTotal++
      exposedSum += t.toolsExposed; calledSum += t.toolsCalled
      cacheRead += t.cacheReadTokens; cacheCreation += t.cacheCreationTokens; inputSum += t.inputTokens
      maxCacheReadTurn = Math.max(maxCacheReadTurn, t.cacheReadTokens)
      const cur = t.activeSkill ?? null
      if (cur) turnsWithSkill++
      if (cur === prev) { if (cur) runLen++ }
      else {
        if (prev) dwellSamples.push(runLen)        // close previous skill segment
        if (cur) { activations++; runLen = 1; boundaryP.push(t.cacheReadTokens) } // open one; record bust cost
        prev = cur
      }
    }
    if (prev) dwellSamples.push(runLen)
  }
  const meanBoundaryP = boundaryP.length ? Math.round(boundaryP.reduce((s, x) => s + x, 0) / boundaryP.length) : 0

  const sorted = [...dwellSamples].sort((a, b) => a - b)
  const pct = p => sorted.length ? sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))] : 0
  const mean = arr => arr.length ? arr.reduce((s, x) => s + x, 0) / arr.length : 0

  return {
    sessions: Object.keys(bySession).length,
    turnsTotal,
    skillActivations: activations,
    turnsWithSkillActive: turnsWithSkill,
    turnsWithoutSkill: turnsTotal - turnsWithSkill,
    dwell: {
      samples: sorted,
      count: sorted.length,
      mean: round(mean(dwellSamples)),
      median: pct(0.5),
      p90: pct(0.9),
      min: sorted[0] ?? 0,
      max: sorted[sorted.length - 1] ?? 0,
    },
    exposure: {
      avgToolsExposed: round(exposedSum / Math.max(1, turnsTotal)),
      avgToolsCalled: round(calledSum / Math.max(1, turnsTotal)),
      calledToExposedRatio: round(calledSum / Math.max(1, exposedSum)),
    },
    cache: {
      totalInputTokens: inputSum,
      cacheReadTokens: cacheRead,
      cacheCreationTokens: cacheCreation,
      cacheHitRate: round(cacheRead / Math.max(1, inputSum)),
      maxCacheReadInOneTurn: maxCacheReadTurn, // conservative P (deepest prefix)
      boundaryP: meanBoundaryP,                // realistic P: prefix at the skill-load turn (where B actually busts)
      note: cacheRead + cacheCreation === 0 ? "provider reported no cache split (expected for non-Anthropic)" : "ok",
    },
  }
}

function verdict(r) {
  const d = r.dwell.mean
  const lines = ["\n================ B GO/NO-GO ================"]
  lines.push(`mean dwell D = ${d} turns/skill-activation (median ${r.dwell.median}, p90 ${r.dwell.p90}, n=${r.dwell.count})`)
  lines.push(`exposed≈${r.exposure.avgToolsExposed} tools/turn, called≈${r.exposure.avgToolsCalled} (ratio ${r.exposure.calledToExposedRatio})`)

  // Cost side, from measured cache. Two P estimates:
  //  - maxP    = deepest cached prefix (conservative; only relevant if the skill loads LATE).
  //  - boundaryP = prefix at the actual skill-load turn (realistic; B only busts there). Skills load
  //    early here, so boundaryP << maxP and the realistic bust is cheap.
  const maxP = r.cache.maxCacheReadInOneTurn
  const boundaryP = r.cache.boundaryP
  const STABLE_CORE = 4, TOK_PER_SCHEMA = 120, CACHE_READ_MULT = 0.1
  const gatedOff = Math.max(0, r.exposure.avgToolsExposed - STABLE_CORE)
  const savePerTurn = round(gatedOff * TOK_PER_SCHEMA * CACHE_READ_MULT) // cache-read savings/turn from gating
  // Bust premium: Anthropic re-creates at 1.25x vs 0.1x read (≈1.15x); DeepSeek re-bills miss vs hit (≈0.9x).
  const bustMult = 0.9
  if (savePerTurn > 0) {
    const dStar = P => round((bustMult * P) / savePerTurn)
    lines.push(`avgToolsExposed=${r.exposure.avgToolsExposed} → gating ${gatedOff} off ⇒ save ${savePerTurn} tok-eq/turn (stable-core ${STABLE_CORE}, ${TOK_PER_SCHEMA} tok/schema)`)
    lines.push(`P: boundary(real, at skill-load) ≈ ${boundaryP} tok · max(deep) ≈ ${maxP} tok · cache-hit ${r.cache.cacheHitRate}`)
    lines.push(`break-even D*: realistic ≈ ${dStar(boundaryP)} turns · conservative(deep) ≈ ${dStar(maxP)} turns  (vs observed D ${d})`)
    const winReal = d >= dStar(boundaryP), winCons = d >= dStar(maxP)
    lines.push(`→ realistic bust: ${winReal ? "✅ GATING WINS" : "❌ loses"} (D ${d} vs D* ${dStar(boundaryP)}) · conservative: ${winCons ? "✅ wins" : "❌ loses"} (D* ${dStar(maxP)})`)
  }
  if (r.dwell.count === 0) lines.push("→ NO skill activations observed — model didn't load skills; dwell not measurable. (Cache numbers above are still valid as the P baseline.)")
  else if (d >= 5) lines.push("→ DWELL SIDE: ✅ high dwell, epoch boundary amortizes — favorable for B with a stable-core.")
  else if (d >= 3) lines.push("→ DWELL SIDE: marginal; B needs a tight stable-core + long-tail-only gating.")
  else lines.push("→ DWELL SIDE: low; epoch gating busts the prefix too often. Prefer A.")
  return lines.join("\n")
}

function strSchema(k, req = true) { return { type: "object", properties: { [k]: { type: "string" } }, ...(req ? { required: [k] } : {}) } }
function round(n) { return Math.round(n * 100) / 100 }
function shortArgs(a) { try { return JSON.stringify(a).slice(0, 80) } catch { return "" } }
function intArg(flag, def) { const i = process.argv.indexOf(flag); return i >= 0 ? Math.max(1, parseInt(process.argv[i + 1], 10) || def) : def }
function strArg(flag, def) { const i = process.argv.indexOf(flag); return i >= 0 && process.argv[i + 1] ? process.argv[i + 1] : def }
function stamp() { return new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14) }
function redact(s) { return s.replace(/sk-[a-zA-Z0-9_*.-]{6,}/g, "sk-[redacted]") }
async function loadSdk() {
  const p = path.join(nodeRoot, "dist", "index.js")
  if (!existsSync(p)) throw new Error(`Node SDK dist not found at ${p}. Run: npm run build --prefix node`)
  return import(pathToFileURL(p).href)
}
function loadEnvFile(fp) {
  if (!existsSync(fp)) return
  for (const raw of readFileSync(fp, "utf8").split(/\r?\n/)) {
    const line = raw.trim(); if (!line || line.startsWith("#")) continue
    const norm = line.startsWith("export ") ? line.slice(7).trim() : line
    const eq = norm.indexOf("="); if (eq <= 0) continue
    const k = norm.slice(0, eq).trim(); let v = norm.slice(eq + 1).trim()
    if ((v.startsWith('"') && v.endsWith('"')) || (v.startsWith("'") && v.endsWith("'"))) v = v.slice(1, -1)
    if (!(k in process.env)) process.env[k] = v
  }
}
