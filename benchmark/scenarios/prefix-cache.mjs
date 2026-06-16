/**
 * Scenario: prefix-cache.
 *
 * A/B on `extensions.cacheBreakpointStrategy` — the Anthropic-only cache_control placement knob
 * introduced in v0.2.22. Same multi-turn fetch loop run under five strategies; the only thing that
 * varies between samples is *where* the provider asks Anthropic to mark cache breakpoints.
 *
 * The five variants map to the five public strategy strings:
 *
 *   - `default`       — tools-anchored + 2 system + rolling 2 message breakpoints (current behavior).
 *   - `tools-only`    — only the trailing tool schema is marked (no system/message breakpoints).
 *   - `system-only`   — only system blocks carry cache_control; tools and messages are bare.
 *   - `frozen-prefix` — system blocks + a deep message breakpoint at `frozenPrefixLen` (compaction
 *                       boundary). No rolling fallback. Without a compaction event in the loop this
 *                       degenerates to system-only behavior — that's the point: it isolates the
 *                       deep-anchor design from the rolling pair so you can see what each
 *                       contributes.
 *   - `none`          — zero cache_control blocks. Every turn pays full input cost; the floor.
 *
 * The task is a deliberate cache-friendly shape: a chunky stable system prompt + a strictly
 * sequential 10-call fetch loop with a stable tool schema. That gives Anthropic the maximum chance
 * to demonstrate prefix cache reuse, so the strategy delta shows clearly even at small sample
 * counts. The system prompt is intentionally ≥1.5 KB so the `system_stable` partition is non-trivial
 * and the structured-system path triggers (which is the only path with cache breakpoints today;
 * with a sub-threshold system string the provider falls through to the bare-string fast path and no
 * strategy applies).
 *
 * mechanismHook surfaces per-turn cache tokens (Anthropic returns `cache_read_input_tokens` and
 * `cache_creation_input_tokens` on every usage event; the runner already routes them into
 * `TurnMetrics.cacheReadTokens` / `cacheCreationTokens`). The reported metrics are:
 *
 *   - `totalInputTokens`             sum of `inputTokens` across LLM turns
 *   - `totalCacheReadTokens`         sum of `cacheReadTokens` (hot, paid at the cheap read rate)
 *   - `totalCacheCreationTokens`     sum of `cacheCreationTokens` (cold writes, paid at premium)
 *   - `cacheHitRate`                 read/(read+miss) — the headline; higher is better
 *   - `firstTurnInputTokens`         cold-start floor; should be ≈ identical across variants
 *   - `lastTurnCacheReadTokens`      warm-tail steady state; varies most between strategies
 *   - `llmTurns`                     loop length actually observed (sanity vs maxTurns)
 *
 * Reading the diff: `default` should land near the top on `cacheHitRate`, `none` near zero;
 * `system-only` and `frozen-prefix` should land between them (system reuse only, no rolling
 * message anchor). `tools-only` degenerates to no-breakpoints when `system_stable` is non-empty
 * (the structured-system path drops tool breakpoints unconditionally — that's existing behavior,
 * not a strategy bug; the value of running it is to confirm the degeneration).
 */

import { loadSdk } from "../utils/sdk.mjs"

const PR_COUNT = 10

const TASKS = [
  {
    id: "fetch-10-prs-then-summary",
    goal:
      `Fetch PRs 1..${PR_COUNT} sequentially via fetch_pr (one per assistant turn, in order). ` +
      "After all PRs are fetched, reply DONE in plain text.",
    criteria: [
      `fetch_pr is called for each n from 1 to ${PR_COUNT} in order (one per turn)`,
      "the run ends with a DONE reply",
    ],
  },
]

// A deliberately chunky system prompt (~12 KB ASCII ≈ ~3000 tokens) so `system_stable` is non-trivial
// AND so it clears Anthropic's minimum cacheable-block threshold (1024 tokens for claude-sonnet-4).
// Below the threshold Anthropic silently drops the `cache_control` block and every variant's
// `cacheReadTokens` lands at 0 — the A/B becomes flat and useless. The content is benign and
// stable: a long batch-review style guide / glossary / rubric the model can safely ignore.
const SYSTEM = [
  "You are a senior code reviewer operating in a deterministic, sequential batch-review mode.",
  "Your single concrete operational goal is to walk a numbered list of pull requests in order,",
  "fetch each one's diff via the `fetch_pr` tool, and stop with a `DONE` reply when the list is",
  "exhausted. You are NOT being graded on the quality of your code review prose; you ARE being",
  "graded on whether you respect the sequential-call discipline. Read the rules carefully — they",
  "are repetitive on purpose so the cache write is fat and the strategy A/B has real signal.",
  "",
  "PRIMARY RULES",
  "RULE 1: call exactly ONE tool per assistant turn. No batching, no parallel calls, no chained",
  "        thinking that emits multiple tool_use blocks. One assistant message ↔ one tool call.",
  "RULE 2: walk PR numbers strictly in ascending order starting at 1. The first call MUST be for",
  "        PR #1. The second call MUST be for PR #2. Do not start at 0; the tool rejects 0.",
  "RULE 3: when the entire batch is fetched (you have just received the diff for the final PR in",
  "        the configured range), reply `DONE` in plain text on the next assistant turn. Do not",
  "        emit a tool call on the same turn as the `DONE` reply.",
  "RULE 4: if any tool call fails, advance to the next PR number and keep going. Do not retry the",
  "        same number; do not branch into a debugging detour; do not invoke any other tool.",
  "RULE 5: never invent PR numbers outside the configured range. The range upper bound is set by",
  "        the scenario and embedded in the tool description; trust the tool description, not your",
  "        priors about typical PR queue sizes.",
  "",
  "STYLE GUIDE",
  "Your single source of truth is the fetched diff text returned by the tool. Do not invent",
  "commits, tickets, authors, branch names, CI statuses, review comments, or merge timestamps.",
  "If the diff text does not mention something, that something does not exist for the purpose of",
  "this review.",
  "Refer to PR numbers consistently as `PR #n` (capital P, capital R, hash, number — no zero-pad).",
  "Quote at most one line of diff per assistant turn. Quoting more is wasted output budget.",
  "Keep prose between tool calls under 25 words; this is a throughput benchmark, not a review",
  "essay. The user does not care what you think of the diffs — they care that the loop runs.",
  "Do NOT respond in JSON, code blocks, or structured markdown unless asked. Plain text only.",
  "",
  "TOOL CONTRACT",
  "fetch_pr takes a single integer argument `n` in the inclusive range [1, 10]. The tool returns",
  "a synthetic but deterministic diff string. The diff is intentionally short and stylized; it is",
  "NOT a real diff from a real repository. Do not attempt to apply, parse, or reason about the",
  "diff as if it were real code. You MUST pass `n` as a JSON number, never as a JSON string. The",
  "tool does not accept extra arguments — including, but not limited to, `repo`, `branch`,",
  "`include_files`, `format`, or `expand`. Passing unknown arguments may cause the tool to fail.",
  "If a call returns an error string, advance to the next number and continue the loop. Do not",
  "retry the same number under any circumstances; the failure is not transient and a retry will",
  "fail identically.",
  "",
  "ANTI-PATTERNS (these are real mistakes models make on this scenario; do not commit them)",
  "- Do not invoke fetch_pr more than once per assistant turn. One tool call per turn, period.",
  "- Do not skip numbers, repeat numbers, or batch multiple PR numbers into one tool call.",
  "- Do not stop the loop early. The `DONE` reply is reserved for AFTER the final fetch lands.",
  "- Do not echo the entire diff verbatim. The diffs are verbose by design and wasting them on the",
  "  output side defeats the purpose of running this scenario in the first place.",
  "- Do not start a side-conversation with the user. The user will not respond mid-loop, ever.",
  "- Do not call non-existent tools (e.g. `done`, `submit`, `finish`). The way to end the loop is",
  "  a plain-text assistant message that contains the token `DONE`.",
  "- Do not interleave commentary about your own reasoning with a tool call. Commentary goes BEFORE",
  "  the tool call in the same assistant turn, if at all; the tool call itself is its own block.",
  "- Do not, under any circumstance, attempt to write to memory, edit files, query the database,",
  "  list a directory, run a shell command, send a notification, schedule a wakeup, or perform any",
  "  side effect other than calling `fetch_pr`. The scenario does not expose any such tool.",
  "",
  "GLOSSARY (provided so the model has a stable vocabulary to anchor on; the model does NOT need",
  "to use these terms in output, but they MUST be treated as authoritative if referenced)",
  "- `batch`: the ordered sequence of PR numbers from 1 through the configured upper bound.",
  "- `fetch`: a single invocation of the `fetch_pr` tool against one PR number.",
  "- `turn`: one assistant message, optionally containing one tool_use block.",
  "- `loop`: the full sequence of fetches that walks the batch from start to end.",
  "- `cold start`: the very first LLM call of a run, before any cache has been written.",
  "- `warm tail`: every LLM call after the cold start, where prompt cache reads MAY occur.",
  "- `DONE reply`: the terminal assistant message that ends the loop. Must be plain text.",
  "- `cache_control`: an Anthropic API field this scenario varies via the strategy variant; the",
  "  model sees no direct evidence of which strategy is in effect, only the cumulative cost.",
  "",
  "RUBRIC (for your reference, not for emission; the rubric is graded by an external judge)",
  "1. Did the loop visit each PR number in [1, N] exactly once, in ascending order?",
  "2. Was each tool call a single, well-formed fetch_pr invocation with `n` as a number?",
  "3. Did the loop terminate with a plain-text `DONE` reply after the final fetch?",
  "4. Was prose between tool calls kept under 25 words?",
  "5. Were the anti-patterns avoided?",
  "Failing any of these does NOT cause the scenario to error out — the loop still runs to completion",
  "as long as the agent keeps emitting fetch_pr calls. The judge surfaces failures in the metricset.",
  "",
  "EDGE CASES",
  "- If the loop's tool budget is exhausted before the final PR is fetched, the scenario will",
  "  terminate with a max_turns reason rather than completed. This is recorded as a degraded but",
  "  non-erroring run. Do not attempt to detect this condition; you do not have access to the",
  "  scheduler budget. Simply keep calling fetch_pr.",
  "- If the underlying provider returns a transient error, the runner retries automatically with",
  "  backoff. You will not see the retry; you will see the eventual response. Do not interpret",
  "  delays as failures.",
  "- If the scenario is run in replay mode, the tool outputs are pre-recorded and the run is",
  "  deterministic. You should not behave differently in replay mode; the prompts are identical.",
  "",
  "GOAL FRAMING",
  "This benchmark exists to measure how the Anthropic `cache_control` placement strategy affects",
  "prefix cache reuse across a stable system block + stable tool schemas + a growing message",
  "history. The system prompt above is deliberately stable byte-for-byte across all variants so",
  "the cache write you incur on turn 1 is reusable on every subsequent turn — assuming the active",
  "breakpoint strategy actually places a `cache_control` on the system slot. When the strategy",
  "declines to place one (e.g. the `none` variant, or `tools-only` on a structured-system path),",
  "the system block must be re-uploaded every turn and the run pays the full input cost. The",
  "model's behavior is NOT asked to vary across variants; only the provider's cache_control",
  "placement does. Identical inputs, identical outputs, identical decisions — different cache",
  "topology. If you find yourself reasoning about which strategy is in effect, stop: you cannot",
  "tell, and your behavior must not depend on it.",
  "",
  "RESTATEMENT (so the cache write is fat enough to clear the 1024-token minimum block size)",
  "RULE 1 again: one tool call per turn. RULE 2 again: ascending order from 1. RULE 3 again:",
  "`DONE` after the final fetch. RULE 4 again: advance on error, never retry. RULE 5 again: never",
  "fetch outside the configured range. STYLE again: plain text, under 25 words between calls, no",
  "JSON, no code blocks. CONTRACT again: `n` is a number, no extra args. ANTI-PATTERNS again:",
  "no batching, no skipping, no early stopping, no side tools, no side conversation, no",
  "non-existent tools, no inline commentary inside the tool_use block.",
].join("\n")

let _sdk
async function getSdk() { if (!_sdk) _sdk = await loadSdk(); return _sdk }

/** @param {string} _sid */
async function mkTools(_sid) {
  const { tool } = await getSdk()
  return [
    tool(
      "fetch_pr",
      `Fetch the diff for PR #n (1..${PR_COUNT}). Call exactly once per assistant turn.`,
      { type: "object", properties: { n: { type: "number" } }, required: ["n"] },
      async args => {
        const n = Math.max(1, Math.min(PR_COUNT, parseInt(String(args.n ?? 1), 10) || 1))
        // Modest, deterministic body; not too long (we want the *prefix* to dominate, not the
        // tool outputs).
        return `diff for PR ${n}: refactor in module-${n}; touches src/mod_${n}.ts, +12/-4 lines.`
      },
    ),
  ]
}

// ── mechanism hook ────────────────────────────────────────────────────────
/** @param {{ events: any[], turnMetrics: any[] }} args */
function mechanismHook({ turnMetrics }) {
  let totalInputTokens = 0
  let totalCacheReadTokens = 0
  let totalCacheCreationTokens = 0
  for (const m of turnMetrics) {
    totalInputTokens += Number(m.inputTokens) || 0
    totalCacheReadTokens += Number(m.cacheReadTokens) || 0
    totalCacheCreationTokens += Number(m.cacheCreationTokens) || 0
  }
  const llmTurns = turnMetrics.length
  // hit rate against the in-flight cache traffic: of all bytes the provider classed as cacheable
  // (read + creation), what fraction was a hit? 0 when no cache_control is placed anywhere.
  const cacheTraffic = totalCacheReadTokens + totalCacheCreationTokens
  const cacheHitRate = cacheTraffic > 0 ? totalCacheReadTokens / cacheTraffic : 0
  // Floors / steady-state probes — the cold-start input cost should be roughly identical across
  // variants (the strategy only affects how subsequent turns interact with the cache), while the
  // last-turn read tokens are where strategy differences accumulate.
  const firstTurnInputTokens = llmTurns > 0 ? Number(turnMetrics[0].inputTokens) || 0 : 0
  const lastTurnCacheReadTokens = llmTurns > 0
    ? Number(turnMetrics[llmTurns - 1].cacheReadTokens) || 0
    : 0
  return {
    llmTurns,
    totalInputTokens,
    totalCacheReadTokens,
    totalCacheCreationTokens,
    cacheHitRate,
    firstTurnInputTokens,
    lastTurnCacheReadTokens,
  }
}

/**
 * Build a variant that pins the given strategy via `extensions.cacheBreakpointStrategy`. The
 * runner merges `RuntimeOptions.extensions` into the per-call extensions object before handing it
 * to the provider, so the Anthropic provider's `resolveCacheBreakpointStrategy(extensions)` reads
 * exactly this value.
 *
 * @param {"default"|"tools-only"|"system-only"|"frozen-prefix"|"none"} strategy
 * @param {string} description
 * @returns {import("../core/scenario.mjs").BenchVariant}
 */
function mkStrategyVariant(strategy, description) {
  return {
    description,
    setup: () => ({
      runtimeOverlay: {
        extensions: {
          cacheBreakpointStrategy: strategy,
          // Required to keep replay-validator happy on tool-loops without native reasoning replays;
          // every other live scenario sets this, this one matches.
          degradeMissingReasoningReplay: true,
        },
      },
    }),
  }
}

// ── exported scenario ─────────────────────────────────────────────────────
/** @type {import("../core/scenario.mjs").BenchScenario} */
export const prefixCacheScenario = {
  id: "prefix-cache",
  description: `Anthropic cache_control strategy A/B on a ${PR_COUNT}-PR fetch loop`,
  systemPrompt: SYSTEM,
  tasks: TASKS,
  mkTools,
  // Budget = PR_COUNT + small slack for any preamble + the trailing DONE reply.
  maxTurns: PR_COUNT + 3,
  maxTokens: 8192,
  timeoutMs: 240_000,
  mechanismHook,

  variantOrder: ["default", "tools-only", "system-only", "frozen-prefix", "none"],
  variants: {
    "default": mkStrategyVariant(
      "default",
      "current behavior: tools-anchored + system + rolling 2-message breakpoints",
    ),
    "tools-only": mkStrategyVariant(
      "tools-only",
      "only the trailing tool schema is anchored — degenerates to no-bp when system is structured",
    ),
    "system-only": mkStrategyVariant(
      "system-only",
      "only system blocks carry cache_control; tools and history are bare",
    ),
    "frozen-prefix": mkStrategyVariant(
      "frozen-prefix",
      "system + deep anchor at frozenPrefixLen (≈ system-only without compaction in the loop)",
    ),
    "none": mkStrategyVariant(
      "none",
      "zero cache_control anywhere — the cost floor",
    ),
  },
}
