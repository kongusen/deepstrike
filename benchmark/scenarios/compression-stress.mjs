/**
 * Scenario: compression-stress.
 *
 * A purpose-built long-loop scenario that stresses the kernel's compression / paging machinery
 * (the "infinite context illusion" §6.1 spec — the second-most-load-bearing OS mechanism after
 * the syscall gate). One task that builds up history monotonically; two variants on the same
 * `maxTokens` budget axis force different compression regimes:
 *
 *   - `budget-loose` (maxTokens = 8192) — enough headroom that compression rarely fires.
 *     Baseline for how much context the task naturally needs.
 *   - `budget-tight` (maxTokens = 2048) — repeatedly trips compression (`snip_compact` →
 *     `micro_compact` → `context_collapse` → `auto_compact`). Measures whether the agent still
 *     finishes the loop after lossy compaction.
 *
 * The variant axis is pure RuntimeOptions overlay — no new SDK ABI. `mechanismHook` walks session
 * events to count compressions by action and the loop-completion shape (PRs fetched, summary
 * produced); the standard `contextHealth` layer already emits `peakInputTokens` and `compressions`.
 */

import { loadSdk } from "../utils/sdk.mjs"

const PR_COUNT = 12

// ── tasks ──────────────────────────────────────────────────────────────────
const TASKS = [
  {
    id: "review-12-prs",
    goal:
      `You are reviewing a batch of ${PR_COUNT} pull requests sequentially. ` +
      `Call \`fetch_pr\` with n = 1, 2, …, ${PR_COUNT} — one per assistant turn, in order. ` +
      `After receiving each diff, briefly note (in your assistant text) what the PR changes. ` +
      `Once you have fetched and noted all ${PR_COUNT}, call \`summarize_findings\` ONCE with a ` +
      `short multi-line summary covering all PRs. Then reply DONE.`,
    criteria: [
      `fetch_pr is called for every n from 1 to ${PR_COUNT}, in order, one per turn`,
      "summarize_findings is called exactly once, after all fetch_pr calls",
      "the final summary mentions at least 8 of the 12 PRs",
    ],
  },
]

const SYSTEM = [
  "You are a senior engineer doing code review across many PRs.",
  "RULE 1: call exactly ONE tool per assistant turn (no batching).",
  "RULE 2: fetch_pr first for all PRs 1..N in order — do NOT call summarize_findings before all fetches finish.",
  "RULE 3: after all fetches, call summarize_findings ONCE with a concise multi-PR summary, then reply DONE.",
].join("\n")

// ── tool factory ───────────────────────────────────────────────────────────
// 12 distinct ~700-char diffs — large enough that history fills 4-10k tokens by the end, so a
// 2k budget repeatedly compacts and an 8k budget rarely does.

const DIFF_FRAGMENTS = [
  ["src/auth.js", "fix token expiry to use UTC clock; previously DST shifts flagged sessions early"],
  ["src/payment.js", "validate charge amount before remote call (was zero-value request causing 500)"],
  ["src/cart.js", "deduplicate line items on add (regression from coupon-stacking landing 2025-12)"],
  ["src/router.js", "extract path-template builder; reduces parse cost on hot path by ~12%"],
  ["src/logging.js", "tag every log line with the request id from AsyncLocalStorage"],
  ["src/cache.js", "fix race in expiry: lru-evict could outrun the timer wheel under load"],
  ["src/oauth.js", "rotate refresh-token only when issuer-rotation header is set, not unconditionally"],
  ["src/migrations/053_orders.sql", "add covering index on orders(user_id, created_at) for hot dashboard query"],
  ["src/jobs/sweeper.js", "make sweeper idempotent; previously double-claimed jobs on driver retry"],
  ["src/tests/payment.test.js", "add table-driven cases for charge_amount edge values (0, max, negative)"],
  ["src/api/users.js", "switch listUsers to keyset pagination; offset performance dropped after 100k rows"],
  ["src/config/loader.js", "fail fast on missing required env vars (was silently using defaults)"],
]

function diffText(n) {
  const i = (n - 1) % DIFF_FRAGMENTS.length
  const [file, oneLine] = DIFF_FRAGMENTS[i]
  // Build ~700 chars of plausible diff body so 12 PRs ≈ 8-9k chars of pure history text.
  const body = [
    `diff --git a/${file} b/${file}`,
    `--- a/${file}`,
    `+++ b/${file}`,
    `@@ -1,12 +1,18 @@`,
    `+// PR ${n}: ${oneLine}`,
    `+// reviewed-by: alice@team`,
    `+// jira: PROJ-${1000 + n}`,
    `+// risk: medium · rollout: feature-flag`,
    `+`,
    ` // existing context line 1`,
    ` // existing context line 2`,
    `-  return legacyImpl(req)`,
    `+  // Touch: ${file} (PR #${n})`,
    `+  // Description: ${oneLine}`,
    `+  // The PR adds telemetry around the changed paths and updates two tests.`,
    `+  // It is gated behind \`feature.${file.split("/").pop().split(".")[0]}_v2\` for the first week.`,
    `+  return newImpl(req, { tracer, featureFlag: "${file}-${n}" })`,
    ` // existing context line 3`,
    ` // existing context line 4`,
    ` // existing context line 5`,
    `@@ -42,6 +48,11 @@`,
    `+  metrics.increment("${file}.touched", { pr: ${n} })`,
    `+  audit.log({ file: "${file}", pr: ${n}, change: ${JSON.stringify(oneLine)} })`,
    `+  // tests added: see PR ${n}'s test file`,
  ].join("\n")
  return body
}

let _sdkCache
async function getSdk() {
  if (!_sdkCache) _sdkCache = await loadSdk()
  return _sdkCache
}

/** @param {string} _sessionId */
async function mkTools(_sessionId) {
  const sdk = await getSdk()
  const { tool } = sdk
  const j = o => JSON.stringify(o)

  return [
    tool(
      "fetch_pr",
      `Fetch the diff for pull request #n (1..${PR_COUNT}). Call exactly once per assistant turn.`,
      { type: "object", properties: { n: { type: "number" } }, required: ["n"] },
      async args => {
        const n = Math.max(1, Math.min(PR_COUNT, parseInt(String(args.n ?? 1), 10) || 1))
        return diffText(n)
      },
    ),
    tool(
      "summarize_findings",
      `Submit the multi-PR summary. Call EXACTLY ONCE after all ${PR_COUNT} fetch_pr calls.`,
      { type: "object", properties: { summary: { type: "string" } }, required: ["summary"] },
      async args => j({ ok: true, length: String(args.summary ?? "").length }),
    ),
  ]
}

// ── mechanism hook ─────────────────────────────────────────────────────────
/** @param {{ events: any[], turnMetrics: any[] }} args */
function mechanismHook({ events }) {
  const compressions = events.filter(e => e.event?.kind === "compressed")
  /** @type {Record<string, number>} */
  const actionCounts = {}
  for (const e of compressions) {
    const a = e.event.action ?? "unknown"
    actionCounts[a] = (actionCounts[a] ?? 0) + 1
  }

  let prCalls = 0
  let summarizeCalls = 0
  for (const e of events) {
    if (e.event?.kind !== "tool_requested") continue
    for (const c of e.event.calls ?? []) {
      if (c.name === "fetch_pr") prCalls++
      else if (c.name === "summarize_findings") summarizeCalls++
    }
  }

  return {
    compressionCount: compressions.length,
    prCallCount: prCalls,
    summarizeCallCount: summarizeCalls,
    actionSnipCompact: actionCounts.snip_compact ?? 0,
    actionMicroCompact: actionCounts.micro_compact ?? 0,
    actionContextCollapse: actionCounts.context_collapse ?? 0,
    actionAutoCompact: actionCounts.auto_compact ?? 0,
    completionRatio: round(Math.min(1, prCalls / PR_COUNT)),
  }
}

function round(n) { return Math.round(n * 100) / 100 }

// ── exported scenario ───────────────────────────────────────────────────────
/** @type {import("../core/scenario.mjs").BenchScenario} */
export const compressionStressScenario = {
  id: "compression-stress",
  description: `Long-loop stress: review ${PR_COUNT} PRs sequentially; A/B on maxTokens budget`,
  systemPrompt: SYSTEM,
  tasks: TASKS,
  mkTools,
  maxTurns: 28,       // tight headroom — at most 12 fetches + 1 summary + slack
  maxTokens: 8192,    // scenario default; variants override
  timeoutMs: 420_000,
  mechanismHook,

  variantOrder: ["budget-loose", "budget-tight"],
  variants: {
    "budget-loose": {
      description: "maxTokens = 8192 — large budget; compression rarely fires (baseline)",
      setup: () => ({
        runtimeOverlay: {
          maxTokens: 8192,
          // DeepSeek's replay validator rejects assistant tool-call turns whose history lacks
          // `reasoning_content`. On a long loop the chat model omits it; degrade lets the run
          // proceed instead of hard-erroring mid-task.
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
    "budget-tight": {
      description: "maxTokens = 2048 — tight budget; compression fires repeatedly",
      setup: () => ({
        runtimeOverlay: {
          maxTokens: 2048,
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
  },
}
