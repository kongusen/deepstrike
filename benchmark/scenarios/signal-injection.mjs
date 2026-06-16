/**
 * Scenario: signal-injection.
 *
 * Measures how the kernel reacts when a `RuntimeSignal` lands mid-loop. The task is the same long
 * fetch loop compression-stress uses (drives a known number of turns), but at turn `INJECT_TURN`
 * each variant's `SignalSource` returns one signal:
 *
 *   - `no-signal` (baseline) — `SignalSource` always returns null; the run finishes normally.
 *   - `soft-interrupt` — `urgency: "high"` `Interrupt` injects a `[SIGNAL]` observation; the
 *     model sees it next turn and CAN choose to wrap up, but the run is NOT preempted.
 *   - `hard-interrupt` — `urgency: "critical"` `InterruptNow` preempts the in-flight LLM call;
 *     the run ends with `status: "user_abort"` (or similar) within ~1 turn of the inject.
 *
 * mechanismHook reports the inject turn, total turns observed (so preemption latency = total -
 * inject), the final status code, and whether the model paid attention to the signal.
 *
 * ⚠ Empirical finding (DeepSeek live, 2026-06-17): merely setting `signalSource` on the runner —
 * even before any signal fires — causes the run to terminate after ~2 LLM turns with no
 * `run_terminal` event (vs. the 13-turn full-loop completion when `signalSource` is absent).
 * The scenario captures this Δ for the time being; understanding/fixing the SDK interaction is
 * tracked separately. Don't read too much into the soft/hard urgency split until that's resolved.
 */

import { loadSdk } from "../utils/sdk.mjs"

const PR_COUNT = 12
const INJECT_TURN = 4 // counter starts at 0; we inject on the Nth call to nextSignal()

const TASKS = [
  {
    id: "fetch-12-prs-then-summary",
    goal:
      `Fetch PRs 1..${PR_COUNT} sequentially via fetch_pr (one per assistant turn, in order). ` +
      "If at any point you see a `[SIGNAL]` note, briefly acknowledge it in plain text but continue " +
      "the loop. After all 12 fetches, reply DONE.",
    criteria: [
      "fetch_pr is called for each n from 1 to 12 in order (one per turn) up to whatever budget the run gets",
      "if a [SIGNAL] note appears, the agent's next assistant text mentions seeing it",
    ],
  },
]

const SYSTEM = [
  "You are reviewing a batch of PRs sequentially.",
  "RULE 1: call exactly ONE tool per assistant turn (no batching).",
  "RULE 2: if you see `[SIGNAL]` in your context, ACKNOWLEDGE it briefly in your assistant text, then KEEP GOING.",
  "RULE 3: when all PRs are fetched, reply DONE in plain text.",
].join("\n")

// ── tool factory ──────────────────────────────────────────────────────────
let _sdk
async function getSdk() { if (!_sdk) _sdk = await loadSdk(); return _sdk }

/** @param {string} _sid */
async function mkTools(_sid) {
  const { tool } = await getSdk()
  return [
    tool(
      "fetch_pr",
      `Fetch the diff for PR #n (1..${PR_COUNT}). Call once per assistant turn.`,
      { type: "object", properties: { n: { type: "number" } }, required: ["n"] },
      async args => {
        const n = Math.max(1, Math.min(PR_COUNT, parseInt(String(args.n ?? 1), 10) || 1))
        return `diff for PR ${n}: minor refactor in module-${n}`
      },
    ),
  ]
}

// ── one-shot signal source: returns the signal on the Nth call to nextSignal() ────────────
/**
 * @param {{ urgency: "low" | "normal" | "high" | "critical", injectAtCall: number,
 *           payloadReason: string }} cfg
 */
function makeOneShotSignalSource(cfg) {
  let calls = 0
  let fired = false
  /** @type {{ calls: number, firedAtCall: number | null }} */
  const stats = { calls: 0, firedAtCall: null }
  return {
    stats,
    async nextSignal() {
      stats.calls = ++calls
      if (!fired && calls >= cfg.injectAtCall) {
        fired = true
        stats.firedAtCall = calls
        return {
          source: { kind: "scenario" },
          signalType: "event",
          urgency: cfg.urgency,
          payload: { reason: cfg.payloadReason, injected_at_call: calls },
          dedupeKey: `bench-signal-${cfg.urgency}-${calls}`,
        }
      }
      return null
    },
  }
}

// ── mechanism hook ────────────────────────────────────────────────────────
/** @param {{ events: any[], turnMetrics: any[] }} args */
function mechanismHook({ events, turnMetrics }) {
  const llmTurns = turnMetrics.length
  // Final status of the run, in numeric form so mean+stdev across samples is meaningful.
  // 1 = completed · 0.66 = max_turns · 0.33 = user_abort/preempted · 0 = run never reached terminal.
  // The session log stores the terminal field as `reason`, not `status`.
  let finalCode = 0
  let runTerminated = 0
  const lastTerminal = [...events].reverse().find(e => (e.event ?? e).kind === "run_terminal")
  if (lastTerminal) {
    runTerminated = 1
    const reason = String((lastTerminal.event ?? lastTerminal).reason ?? "").toLowerCase()
    if (reason.includes("complete")) finalCode = 1
    else if (reason.includes("max_turns")) finalCode = 0.66
    else if (reason.includes("abort") || reason.includes("preempt") || reason.includes("user_abort")) finalCode = 0.33
  }

  // Count fetch_pr calls actually executed (kernel-approved).
  let fetchCount = 0
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind !== "tool_requested") continue
    for (const c of ev.calls ?? []) if (c.name === "fetch_pr") fetchCount++
  }

  // Did the model verbally acknowledge a [SIGNAL] note? Cheap heuristic: any assistant text that
  // mentions "SIGNAL" or "signal" after the inject window. Reads llm_completed.content directly.
  let signalAcknowledged = 0
  for (const e of events) {
    const ev = e.event ?? e
    if (ev.kind === "llm_completed" && typeof ev.content === "string"
        && /\[?SIGNAL\]?|signal\b/i.test(ev.content)) {
      signalAcknowledged = 1
      break
    }
  }

  return {
    llmTurns,
    fetchCount,
    finalStatusCode: finalCode,
    runTerminated,
    signalAcknowledged,
  }
}

// ── exported scenario ─────────────────────────────────────────────────────
/** @type {import("../core/scenario.mjs").BenchScenario} */
export const signalInjectionScenario = {
  id: "signal-injection",
  description: `Long-loop ${PR_COUNT}-PR fetch; A/B on signal urgency injected at turn ${INJECT_TURN}`,
  systemPrompt: SYSTEM,
  tasks: TASKS,
  mkTools,
  maxTurns: PR_COUNT + 4, // budget for 12 fetches + a few "DONE" turns + signal slack
  maxTokens: 8192,
  timeoutMs: 240_000,
  mechanismHook,

  variantOrder: ["no-signal", "soft-interrupt", "hard-interrupt"],
  variants: {
    "no-signal": {
      description: "no signal injected — baseline run completes the full loop",
      setup: () => ({
        runtimeOverlay: { extensions: { degradeMissingReasoningReplay: true } },
      }),
    },
    "soft-interrupt": {
      description: `urgency=high Interrupt at turn ${INJECT_TURN} — kernel injects [SIGNAL] note, run continues`,
      setup: () => ({
        runtimeOverlay: {
          signalSource: makeOneShotSignalSource({
            urgency: "high",
            injectAtCall: INJECT_TURN,
            payloadReason: "scenario_soft_interrupt",
          }),
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
    "hard-interrupt": {
      description: `urgency=critical InterruptNow at turn ${INJECT_TURN} — preempts in-flight LLM call`,
      setup: () => ({
        runtimeOverlay: {
          signalSource: makeOneShotSignalSource({
            urgency: "critical",
            injectAtCall: INJECT_TURN,
            payloadReason: "scenario_hard_interrupt",
          }),
          extensions: { degradeMissingReasoningReplay: true },
        },
      }),
    },
  },
}
