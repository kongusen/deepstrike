/**
 * format-discipline — a small honest live task set for exercising the self-harness loop for real.
 *
 * All five tasks share one underlying failure mechanism: models tend to wrap deliverables in
 * markdown fences and surround them with prose, violating strict output-format constraints. A weak
 * generic seed harness fails a subset; a harness edit that installs output/verification discipline
 * should lift BOTH splits — exactly the "reusable mechanism, not per-task patch" the promotion rule
 * is meant to select for. Held-in: json-strict / word-limit / checklist. Held-out: csv-strict /
 * summary-limit.
 *
 * Usage:
 *   node benchmark/selfharness/cli.mjs --adapter ./benchmark/selfharness/adapters/format-discipline.mjs \
 *        --held-in json-strict,word-limit,checklist --held-out csv-strict,summary-limit \
 *        --rounds 2 --k 2 --provider deepseek
 */

import { createLiveAdapter } from "./live.mjs"

const TASKS = [
  {
    id: "json-strict",
    goal: "Return ONLY a JSON object with keys name (string), version (string), and layers (array of exactly 3 strings) describing a fictional network protocol stack. No markdown fences, no explanation, no text before or after the JSON.",
    criteria: [
      { id: "J_bare", text: "the output is a bare JSON object with no markdown code fences and no prose before or after it" },
      { id: "J_shape", text: "the JSON has exactly the keys name, version, layers, and layers is an array of exactly 3 strings" },
    ],
  },
  {
    id: "word-limit",
    goal: "Explain TCP slow start in exactly one sentence of at most 25 words.",
    criteria: [
      { id: "W_one", text: "the answer is exactly one sentence" },
      { id: "W_limit", text: "the answer contains at most 25 words" },
    ],
  },
  {
    id: "checklist",
    goal: "List 5 steps to safely rotate an API key, numbered 1-5. Output the numbered list only — no introduction, no closing remarks.",
    criteria: [
      { id: "C_five", text: "the output contains exactly 5 numbered steps" },
      { id: "C_bare", text: "there is no introductory or closing text outside the numbered list" },
    ],
  },
  {
    id: "csv-strict",
    goal: "Output a CSV table with header row city,country and exactly 3 data rows. Nothing else: no markdown fences, no commentary.",
    criteria: [
      { id: "V_shape", text: "the output is a valid CSV with header city,country and exactly 3 data rows" },
      { id: "V_bare", text: "there are no markdown fences and no text outside the CSV" },
    ],
  },
  {
    id: "summary-limit",
    goal: "Summarize the tradeoff between polling and webhooks in at most 40 words, as plain prose without bullet points.",
    criteria: [
      { id: "S_limit", text: "the answer contains at most 40 words" },
      { id: "S_prose", text: "the answer is plain prose with no bullet points or numbered lists" },
    ],
  },
]

/** @param {{ providerDesc: any, judgeProviderDesc?: any }} ctx */
export function createAdapter(ctx) {
  return createLiveAdapter({
    providerDesc: ctx.providerDesc,
    judgeProviderDesc: ctx.judgeProviderDesc,
    tasks: TASKS,
    maxTurns: 4,
    timeoutMs: 180_000,
  })
}

/** Weak generic seed — the paper's "minimal initial harness" analog. */
export function seedManifest() {
  return {
    manifestVersion: 1,
    parent: null,
    modelProfile: "deepseek/deepseek-chat",
    instructions: { execution: "Answer the user's request helpfully." },
    editableSurfaces: [
      "instructions.bootstrap",
      "instructions.execution",
      "instructions.verification",
      "instructions.failureRecovery",
      "nudges",
      "runtime.criteriaGate",
    ],
    audit: { round: 0, createdBy: "seed" },
  }
}
