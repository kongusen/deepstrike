/**
 * Scenario: gating-dwell.
 *
 * Productised form of node/examples/tool-gating-dwell.mjs. Runs 4 realistic dev tasks against a
 * ~30-tool surface with a 4-skill catalog (debugging / code-review / testing / refactoring).
 * Variants:
 *   - off:  baseline — skills declare NO allowed_tools, no stableCoreToolIds. Full tool set every turn.
 *   - on:   gating — skills declare canonical allowed_tools + stableCoreToolIds wired in. Kernel
 *           narrows the per-turn toolset once a skill is active.
 *
 * mechanismHook emits per-session dwell + exposure + cache-prefix metrics (the things gating's
 * go/no-go decision actually depends on); the aggregator turns them into a `mechanism` layer with
 * mean + stdev across sessions. Both variants run the same tasks/tools/prompt — the ONLY
 * difference between off/on is the skill file frontmatter + stableCoreToolIds.
 */

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

import { loadSdk } from "../utils/sdk.mjs"

const STABLE_CORE = ["read_file", "list_dir", "run_bash"]

const SKILL_DEFS = [
  {
    name: "debugging",
    description: "Systematic root-cause debugging",
    when_to_use: "Reproduce, isolate, read the failing code, form a hypothesis, verify with tests, then fix.",
    body: "# debugging\n\nSystematic root-cause debugging: reproduce, isolate, read the failing code, form a hypothesis, verify with tests, then fix.\n",
    canonicalTools: ["read_file", "search_code", "run_tests", "run_bash", "git_diff", "grep_files"],
  },
  {
    name: "code-review",
    description: "Multi-axis code review",
    when_to_use: "Review correctness, security, performance, style across changed files.",
    body: "# code-review\n\nMulti-axis review: correctness, security, performance, style. Read the diff and the touched files, then report findings.\n",
    canonicalTools: ["git_diff", "read_file", "git_log", "git_blame", "lint", "find_references"],
  },
  {
    name: "testing",
    description: "Test-driven development",
    when_to_use: "Write a failing test first, then make it pass, then refactor.",
    body: "# testing\n\nTest-driven development: write a failing test first, run it, implement the minimum to pass, run again, then refactor.\n",
    canonicalTools: ["run_tests", "read_file", "write_file", "coverage_report", "list_tests"],
  },
  {
    name: "refactoring",
    description: "Safe incremental refactoring",
    when_to_use: "Behavior-preserving changes, one at a time, with tests after each.",
    body: "# refactoring\n\nSafe incremental refactoring: one behavior-preserving change at a time, run tests after each.\n",
    canonicalTools: ["read_file", "write_file", "search_code", "run_tests", "rename_symbol", "replace_in_file"],
  },
]

const TASKS = [
  { id: "debug", goal: "First, call the `skill` tool to load the most relevant skill. Then: A test in tests/auth.test.js is failing. Investigate with the tools, find the root cause, and fix it. Run the tests until they pass.", criteria: ["the failing test is identified, fixed, and the suite passes"] },
  { id: "review", goal: "First, call the `skill` tool to load the most relevant skill. Then: Review the recent changes (use git_diff and read the touched files) for correctness, security, and style. Report findings.", criteria: ["the diff is reviewed across multiple axes"] },
  { id: "test", goal: "First, call the `skill` tool to load the most relevant skill. Then: Add unit tests for the parseConfig function in src/config.js using a test-driven approach. Run the tests.", criteria: ["a failing test is added first, then made to pass"] },
  { id: "refactor", goal: "First, call the `skill` tool to load the most relevant skill. Then: Refactor src/loader.js to improve readability without changing behavior. Verify with the test suite after changes.", criteria: ["the refactor preserves behavior and tests pass"] },
]

const SYSTEM = [
  "You are a senior engineer working in a code repo via tools.",
  "MANDATORY FIRST STEP: your very first action MUST be a call to the `skill` tool with the single",
  "most relevant skill name for the task (one of: debugging, code-review, testing, refactoring).",
  "Do NOT call any other tool until you have loaded a skill. After loading it, follow its guidance.",
  "Then do real multi-step work: investigate with tools across several turns before concluding. ~1 tool per turn.",
  "When the task is done, reply with a short plain-text summary (no tool call).",
].join("\n")

// ── tools: ~30 realistic dev surface, mock outputs ──────────────────────────
// Lazy: don't import the SDK at module load (so `bench list` works without `npm run build`).
let _sdkCache
async function getSdk() {
  if (!_sdkCache) _sdkCache = await loadSdk()
  return _sdkCache
}

/** @param {string} sessionId */
async function mkTools(sessionId) {
  const sdk = await getSdk()
  const { tool } = sdk
  const j = o => JSON.stringify(o)
  const strSchema = (k, req = true) => ({
    type: "object",
    properties: { [k]: { type: "string" } },
    ...(req ? { required: [k] } : {}),
  })

  let testRuns = 0
  return [
      tool("read_file", "Read a source file.", strSchema("path"), async a => `// ${a.path}\nexport function handler(req){ /* ...40 lines... */ return req }\n`),
      tool("list_dir", "List files in a directory.", strSchema("path"), async a => j({ path: a.path, entries: ["index.js", "auth.js", "config.js", "loader.js", "auth.test.js"] })),
      tool("search_code", "Search the codebase for a string.", strSchema("query"), async a => j({ query: a.query, matches: [{ file: "src/auth.js", line: 42 }, { file: "src/config.js", line: 7 }] })),
      tool("run_tests", "Run the test suite (optionally a target).", strSchema("target", false), async () => {
        testRuns++
        return testRuns < 3
          ? j({ passed: 11, failed: 1, failing: [{ test: "auth handles expired token", file: "tests/auth.test.js", line: 18, error: "expected 401, got 500" }] })
          : j({ passed: 12, failed: 0 })
      }),
      tool("run_bash", "Run a shell command.", strSchema("cmd"), async a => `$ ${a.cmd}\n(ok)`),
      tool("git_diff", "Show the working-tree diff.", { type: "object", properties: {} }, async () => "diff --git a/src/payment.js b/src/payment.js\n+ charge(amount) { /* no input validation */ }\n"),
      tool("git_log", "Show recent commits.", { type: "object", properties: {} }, async () => "abc123 fix auth\ndef456 add payment\n"),
      tool("lint", "Lint a file or the project.", strSchema("path", false), async a => j({ path: a.path ?? ".", warnings: 2, errors: 0 })),
      tool("format", "Format a file.", strSchema("path", false), async a => j({ path: a.path ?? ".", formatted: true })),
      tool("write_file", "Write content to a file.", { type: "object", properties: { path: { type: "string" }, content: { type: "string" } }, required: ["path"] }, async a => j({ path: a.path, written: true })),
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

// ── skill file writer (variant-controlled) ──────────────────────────────────
/** @param {string} dir @param {boolean} withAllowedTools */
function writeSkillFiles(dir, withAllowedTools) {
  mkdirSync(dir, { recursive: true })
  for (const skill of SKILL_DEFS) {
    const allowedLine = withAllowedTools ? `allowed_tools: ${skill.canonicalTools.join(", ")}\n` : ""
    const content = `---
name: ${skill.name}
description: ${skill.description}
when_to_use: ${skill.when_to_use}
${allowedLine}effort: 2
---

${skill.body}`
    writeFileSync(path.join(dir, `${skill.name}.md`), content)
  }
}

// ── mechanism hook: per-session dwell / exposure / cache-prefix stats ───────
/** @param {{ events: any[], turnMetrics: any[] }} args */
function mechanismHook({ turnMetrics }) {
  if (turnMetrics.length === 0) return {}

  const exposed = turnMetrics.map(t => t.toolsExposed || 0)
  const called = turnMetrics.map(t => t.toolsCalled || 0)
  const avgToolsExposed = mean(exposed)
  const avgToolsCalled = mean(called)
  const exposedSum = exposed.reduce((s, x) => s + x, 0)
  const calledSum = called.reduce((s, x) => s + x, 0)
  const calledToExposedRatio = exposedSum > 0 ? calledSum / exposedSum : 0

  // Dwell: max consecutive run-length of identical activeSkill (null counts as "no skill").
  /** @type {number[]} */
  const dwellSamples = []
  let activations = 0
  let prev
  let runLen = 0
  /** @type {number[]} */
  const boundaryP = []
  for (const t of turnMetrics) {
    const cur = t.activeSkill ?? null
    if (cur === prev) { if (cur) runLen++ }
    else {
      if (prev) dwellSamples.push(runLen)
      if (cur) {
        activations++
        runLen = 1
        boundaryP.push(t.cacheReadTokens || 0)
      }
      prev = cur
    }
  }
  if (prev) dwellSamples.push(runLen)

  const maxCacheReadInOneTurn = Math.max(0, ...turnMetrics.map(t => t.cacheReadTokens || 0))
  const meanBoundaryP = boundaryP.length ? mean(boundaryP) : 0

  return {
    avgToolsExposed: round(avgToolsExposed),
    avgToolsCalled: round(avgToolsCalled),
    calledToExposedRatio: round(calledToExposedRatio),
    skillActivations: activations,
    dwellMean: round(mean(dwellSamples)),
    dwellMax: dwellSamples.length ? Math.max(...dwellSamples) : 0,
    boundaryP: round(meanBoundaryP),
    maxCacheReadInOneTurn,
  }
}

function mean(arr) { return arr.length ? arr.reduce((s, x) => s + x, 0) / arr.length : 0 }
function round(n) { return Math.round(n * 100) / 100 }

// ── exported scenario ───────────────────────────────────────────────────────
/** @type {import("../core/scenario.mjs").BenchScenario} */
export const gatingDwellScenario = {
  id: "gating-dwell",
  description: "P0-C/P1-B gating evaluation: 4 dev tasks × ~30 tools × 4 skills",
  systemPrompt: SYSTEM,
  tasks: TASKS,
  skills: SKILL_DEFS,
  mkTools,
  maxTurns: 12,
  maxTokens: 60_000,
  timeoutMs: 300_000,
  mechanismHook,

  variantOrder: ["off", "on"],
  variants: {
    off: {
      description: "no gating — skills carry no allowed_tools, no stable-core",
      setup: (_scenario, ctx) => {
        const dir = mkdtempSync(path.join(tmpdir(), `bench-${ctx.scenarioId}-off-`))
        writeSkillFiles(dir, false)
        return {
          runtimeOverlay: { skillDir: dir },
          cleanup: () => { try { rmSync(dir, { recursive: true, force: true }) } catch {} },
        }
      },
    },
    on: {
      description: "gating on — skills declare allowed_tools + stable-core wired in",
      setup: (_scenario, ctx) => {
        const dir = mkdtempSync(path.join(tmpdir(), `bench-${ctx.scenarioId}-on-`))
        writeSkillFiles(dir, true)
        return {
          runtimeOverlay: { skillDir: dir, stableCoreToolIds: STABLE_CORE },
          cleanup: () => { try { rmSync(dir, { recursive: true, force: true }) } catch {} },
        }
      },
    },
  },
}
