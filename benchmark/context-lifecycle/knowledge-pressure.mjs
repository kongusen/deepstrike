#!/usr/bin/env node
// Live knowledge-pressure task for plan task 7.0: a ledger audit whose rules live only in memory,
// under a knowledge budget too small for everything the kernel recalls.
//
//   node benchmark/context-lifecycle/knowledge-pressure.mjs [--repo <checkout>] [--variant <name>]
//        [--policy '<ContextPolicy overrides JSON>'] [--tasks N] [--start N] [--pages N]
//        [--budget <knowledge budget ratio>] [--top-k N] [--provider deepseek|openai]
//        [--max-tokens N] [--out trials.jsonl]
//
// The kernel recalls `--top-k` (default 12) memory index lines at run start: two rules the answer
// needs (REVIEW-HOLD counts as REVIEW; ids ending in 0 are voided) and ten distractors, several of
// them about other audits and ledgers so lexical evidence alone cannot tell them apart. An index
// line carries only a description; the model must read a rule with `memory(record_id)` to learn
// it. The knowledge budget (`--budget`, default 0.04 of the window) holds a few lines, so the
// kernel evicts at every boundary, and the window (`--max-tokens`, default 7000) compacts the
// history that holds the rules' full text. What the run keeps, evicts, recalls again and reads
// back is the measurement: regret, waste and read-backs per trial. Graded by exact match.
// Writes one trial per task for `deepstrike-lab`; never prints a key.

import { resolve } from "node:path"
import { MeteredProvider, here, loadEnv, parseArgs, realProvider, recordingSessionLog, redact, sdk, trialFrom, writeTrials } from "./trial.mjs"

const arg = parseArgs()
const repo = resolve(arg("--repo", here))
const variant = arg("--variant", "default")
const policy = JSON.parse(arg("--policy", "{}"))
const taskCount = Number(arg("--tasks", "6"))
const maxTokens = Number(arg("--max-tokens", "7000"))
const budget = Number(arg("--budget", "0.04"))
const topK = Number(arg("--top-k", "12"))
const out = arg("--out")

await loadEnv()
const dist = sdk(repo)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const { provider: inner, model } = await realProvider(dist, arg("--provider", "deepseek"))
const provider = new MeteredProvider(inner)

// ── fixtures ───────────────────────────────────────────────────────────────────────────────────

const PAGES = Number(arg("--pages", "6"))
const ROWS = 24
const ACCOUNTS = 5
const FLAGS = ["OK", "REVIEW", "OK", "REVIEW-HOLD", "OK", "OK", "REVIEW"]
const row = (page, i) => {
  const n = page * ROWS + i
  return {
    id: `T${String(n).padStart(4, "0")}`,
    account: `ACC-${(n * 7 + page) % ACCOUNTS + 1}`,
    cents: 1000 + (n * 7919) % 90000,
    flag: FLAGS[(n * 3 + page) % FLAGS.length],
  }
}
const ledgerPage = page => `transaction ledger page ${page} of ${PAGES}\n` + Array.from({ length: ROWS }, (_, i) => {
  const r = row(page, i)
  return `${r.id} | ${r.account} | ${(r.cents / 100).toFixed(2)} USD | ${r.flag}`
}).join("\n")
const counted = r => (r.flag === "REVIEW" || r.flag === "REVIEW-HOLD") && !r.id.endsWith("0")
const expectedTotal = account => {
  let cents = 0
  for (let page = 1; page <= PAGES; page++) {
    for (let i = 0; i < ROWS; i++) {
      const r = row(page, i)
      if (r.account === account && counted(r)) cents += r.cents
    }
  }
  return (cents / 100).toFixed(2)
}

const scope = { tenant_id: "bench", namespace: "knowledge-pressure" }
const record = (id, description, content) => ({
  record_id: id, scope, name: id, kind: "project", content, description,
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
})
/** The two records the answer needs. Their descriptions name the topic, not the rule. */
const REQUIRED = [
  record("ledger-flag-rule", "transaction ledger audit: which flags count as REVIEW",
    "In the transaction ledger audit, a transaction flagged REVIEW-HOLD counts as REVIEW. Include both flags in audit totals."),
  record("ledger-void-rule", "transaction ledger audit: voided transactions",
    "In the transaction ledger audit, a transaction whose id ends in 0 is voided. Leave voided transactions out of every audit total, whatever their flag."),
]
/** Distractors: other audits and ledgers that share words with the task, and unrelated notes. */
const DISTRACTORS = [
  record("payroll-audit-flags", "payroll audit: which overtime flags need approval",
    "In the payroll audit, overtime flagged OT-PENDING needs a manager's approval before payout."),
  record("inventory-ledger-adjust", "inventory ledger: stock count adjustments",
    "Inventory ledger adjustments under 5 units are booked without a second count."),
  record("expense-audit-receipts", "expense audit: receipt thresholds per transaction",
    "Expense audit: transactions above 75 USD need an itemized receipt."),
  record("vendor-ledger-terms", "vendor ledger: payment terms by account",
    "Vendor ledger accounts settle net 30 unless the contract says net 45."),
  record("bank-recon-timing", "bank reconciliation: timing differences in the ledger",
    "Bank reconciliation treats deposits in transit for up to 3 business days as timing differences."),
  record("tax-ledger-vat", "tax ledger: VAT rounding per transaction",
    "VAT is rounded half-up per line in the tax ledger, not per invoice."),
  record("audit-committee", "audit committee meeting schedule",
    "The audit committee meets on the second Tuesday of each quarter."),
  record("ledger-export", "ledger export format for the finance team",
    "Ledger exports go to finance as UTF-8 CSV with a header row."),
  record("transaction-id-format", "transaction id format across systems",
    "Transaction ids are T followed by four digits in the ledger and TX- followed by eight digits in the bank feed."),
  record("deploy-freeze", "the Mercury deploy freeze calendar",
    "Mercury deploys are frozen from Thursday 18:00 to Monday 09:00 during release weeks."),
]
const MEMORIES = [...REQUIRED, ...DISTRACTORS]
function memoryStore() {
  const words = text => new Set(String(text).toLowerCase().match(/[a-z0-9-]+/g) ?? [])
  return {
    put: async () => {}, delete: async () => {}, saveSession: async () => {},
    get: async (_agent, id) => MEMORIES.find(memory => memory.record_id === id) ?? null,
    search: async (_agent, query) => {
      const asked = words(query.query ?? query.text ?? "")
      return MEMORIES
        .map(memory => ({ record: memory, score: [...words(`${memory.description} ${memory.content}`)].filter(w => asked.has(w)).length / 10, why: "lexical" }))
        .filter(hit => hit.score > 0)
        .sort((a, b) => b.score - a.score || a.record.record_id.localeCompare(b.record.record_id))
        .slice(0, query.top_k ?? topK)
    },
  }
}

/** Ground truth the kernel cannot see: which recalled items the answer needed. Counted from the
 *  run's knowledge item records, so a change to the kernel's reference evidence cannot grade
 *  itself. Every count is better lower. */
function oracle(lifecycle) {
  const required = new Set(REQUIRED.map(memory => `memory:${memory.record_id}`))
  const distractor = new Set(DISTRACTORS.map(memory => `memory:${memory.record_id}`))
  const items = lifecycle.flatMap(report => report.items ?? [])
  const of = keys => items.filter(item => keys.has(item.key))
  const refs = item => item.value?.ref_count ?? 0
  return {
    // A required rule that left the context: the run had to rely on history or luck after.
    required_dropped: of(required).filter(item => item.end === "evicted" || item.end === "expired").length,
    // A required rule the kernel saw no use of: its retention rests on nothing.
    required_unreferenced: [...required].filter(key => !of(new Set([key])).some(item => refs(item) > 0)).length,
    // Uses the kernel credited to noise: evidence that keeps distractors over the rules.
    distractor_refs: of(distractor).reduce((sum, item) => sum + refs(item), 0),
    // Noise still resident when the run ended.
    distractor_resident_tokens: of(distractor).filter(item => !item.end).reduce((sum, item) => sum + item.tokens, 0),
  }
}

// ── one task ───────────────────────────────────────────────────────────────────────────────────

async function runTask(index) {
  const account = `ACC-${(index % ACCOUNTS) + 1}`
  const expected = expectedTotal(account)
  provider.reset()
  const recorder = recordingSessionLog(new InMemorySessionLog())
  const plane = new LocalExecutionPlane()
  plane.register(root.tool(
    "read_ledger",
    `Read one page of the transaction ledger (pages 1 to ${PAGES}).`,
    { type: "object", properties: { page: { type: "integer", minimum: 1, maximum: PAGES } }, required: ["page"] },
    async ({ page }) => ledgerPage(Math.min(PAGES, Math.max(1, Number(page) || 1))),
  ))
  const runner = new RuntimeRunner({
    provider,
    sessionLog: recorder.log,
    executionPlane: plane,
    maxTokens,
    maxTurns: 40,
    maxTotalTokens: 400_000,
    baselineToolIds: ["read_ledger"],
    enablePlanTool: true,
    agentId: "ledger-auditor",
    memoryScope: scope,
    memoryStore: memoryStore(),
    memoryPolicy: { retrievalTopK: topK },
    knowledgeBudgetRatio: budget,
    ...(Object.keys(policy).length ? { contextPolicy: policy } : {}),
  })
  const goal = `Transaction ledger audit: read all ${PAGES} pages with read_ledger, one page per call, and add up the amounts of every ${account} transaction flagged REVIEW, applying the transaction ledger audit rules in your memory (they say which other flags count as REVIEW and which transactions to leave out). Keep a running total in your plan as you go. Reply with the final total only, as a number with two decimals.`
  let answer = ""
  let error
  try {
    for await (const event of runner.run({ sessionId: `knowledge-${index}`, goal })) {
      if (event.type === "text_delta") answer += event.delta
    }
  } catch (cause) {
    error = redact(cause?.message ?? cause)
  }
  error ??= provider.lastError
  const terminal = [...recorder.events].reverse().find(event => event.kind === "run_terminal")
  if (!error && terminal?.reason && terminal.reason !== "completed") error = `termination: ${terminal.reason}`
  if (process.env.KNOWLEDGE_DEBUG) {
    const { writeFile } = await import("node:fs/promises")
    await writeFile(process.env.KNOWLEDGE_DEBUG, JSON.stringify({ events: recorder.events, lastContext: provider.lastContext }, null, 1))
  }
  const success = answer.replace(/,/g, "").includes(expected)
  const trial = trialFrom({ scenario: "knowledge-pressure", variant, task: `${account}#${index}`, success, error, provider, events: recorder.events })
  return { ...trial, oracle: oracle(trial.lifecycle), expected, answer: answer.slice(-80) }
}

// Each trial is written as soon as it finishes, so an interrupted run keeps what it measured.
const trials = []
const start = Number(arg("--start", "0"))
for (let index = start; index < start + taskCount; index++) {
  const trial = await runTask(index)
  trials.push(trial)
  await writeTrials(out, [trial])
  const last = trial.lifecycle.at(-1) ?? {}
  const truth = Object.entries(trial.oracle).map(([key, value]) => `${key} ${value}`).join(", ")
  console.error(`${trial.task}: ${trial.success ? "correct" : "wrong"} ${trial.cost.calls} calls regret ${last.regret ?? 0} waste ${last.waste_tokens ?? 0}; ${truth}${trial.error ? ` (${trial.error})` : ""}`)
}
console.error(`${variant} on ${model}: ${trials.filter(trial => trial.success).length}/${trials.length} correct`)
