#!/usr/bin/env node
// Live long task for plan task 4.3: a ledger audit that does not fit the context window.
//
//   node benchmark/context-lifecycle/long-task.mjs [--repo <checkout>] [--variant <name>]
//        [--policy '<ContextPolicy overrides JSON>'] [--tasks N] [--provider deepseek|openai]
//        [--max-tokens N] [--out trials.jsonl]
//
// The model reads a 12-page ledger one page at a time with `read_ledger` and reports the total of
// one account's flagged transactions. The window (`--max-tokens`, default 8000) holds a few pages,
// so the run compacts. The answer depends on a rule only memory holds (REVIEW-HOLD counts as
// REVIEW): the kernel recalls it at run start, and losing it costs the answer. Three knowledge
// notes are seeded, one relevant and two not, so waste is measurable. Graded by exact match.
// Writes one trial per task for `deepstrike-lab`; never prints a key.

import { resolve } from "node:path"
import { MeteredProvider, here, loadEnv, parseArgs, realProvider, recordingSessionLog, redact, sdk, trialFrom, writeTrials } from "./trial.mjs"

const arg = parseArgs()
const repo = resolve(arg("--repo", here))
const variant = arg("--variant", "default")
const policy = JSON.parse(arg("--policy", "{}"))
const taskCount = Number(arg("--tasks", "6"))
const maxTokens = Number(arg("--max-tokens", "8000"))
const out = arg("--out")

await loadEnv()
const dist = sdk(repo)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const { provider: inner, model } = await realProvider(dist, arg("--provider", "deepseek"))
const provider = new MeteredProvider(inner)

// ── fixtures ───────────────────────────────────────────────────────────────────────────────────

const PAGES = 12
const ROWS = 30
const ACCOUNTS = 6
const FLAGS = ["OK", "OK", "OK", "REVIEW", "OK", "REVIEW-HOLD", "OK"]
const row = (page, i) => {
  const n = page * ROWS + i
  return {
    id: `T${String(n).padStart(4, "0")}`,
    account: `ACC-${(n * 7 + page) % ACCOUNTS + 1}`,
    cents: 1000 + (n * 7919) % 90000,
    flag: FLAGS[(n * 3 + page) % FLAGS.length],
  }
}
const ledgerPage = page => `ledger page ${page} of ${PAGES}\n` + Array.from({ length: ROWS }, (_, i) => {
  const r = row(page, i)
  return `${r.id} | ${r.account} | ${(r.cents / 100).toFixed(2)} USD | ${r.flag}`
}).join("\n")
const expectedTotal = account => {
  let cents = 0
  for (let page = 1; page <= PAGES; page++) {
    for (let i = 0; i < ROWS; i++) {
      const r = row(page, i)
      if (r.account === account && (r.flag === "REVIEW" || r.flag === "REVIEW-HOLD")) cents += r.cents
    }
  }
  return (cents / 100).toFixed(2)
}

const scope = { tenant_id: "bench", namespace: "ledger-audit" }
const record = (id, name, description, content) => ({
  record_id: id, scope, name, kind: "project", content, description,
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
})
const MEMORIES = [
  record("audit-rule", "audit-flag-rule", "ledger audit rule: which flags count as REVIEW",
    "For ledger audit totals, a transaction flagged REVIEW-HOLD counts as REVIEW. Include both flags."),
  record("deploy-freeze", "deploy-freeze", "the Mercury deploy freeze calendar",
    "Mercury deploys are frozen from Thursday 18:00 to Monday 09:00 during release weeks."),
  record("wifi", "office-wifi", "office wifi rotation",
    "The office wifi password rotates on the first business day of each month."),
]
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
        .sort((a, b) => b.score - a.score)
        .slice(0, query.top_k ?? 3)
    },
  }
}
const NOTES = [
  "Ledger amounts are in US dollars with two decimals; report totals the same way.",
  "The cafeteria menu changes every Tuesday and Thursday.",
  "Parking level B2 is reserved for visitors during the quarterly review.",
]

// ── one task ───────────────────────────────────────────────────────────────────────────────────

async function runTask(index) {
  const account = `ACC-${(index % ACCOUNTS) + 1}`
  const expected = expectedTotal(account)
  provider.reset()
  const recorder = recordingSessionLog(new InMemorySessionLog())
  const plane = new LocalExecutionPlane()
  plane.register(root.tool(
    "read_ledger",
    "Read one page of the ledger (pages 1 to 12).",
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
    initialMemory: NOTES,
    ...(Object.keys(policy).length ? { contextPolicy: policy } : {}),
  })
  const goal = `Audit the ledger: read all ${PAGES} pages with read_ledger, one page per call, and add up the amounts of every ${account} transaction that the ledger audit rule counts as REVIEW. Keep a running total in your plan as you go. Reply with the final total only, as a number with two decimals.`
  let answer = ""
  let error
  try {
    for await (const event of runner.run({ sessionId: `ledger-${index}`, goal })) {
      if (event.type === "text_delta") answer += event.delta
    }
  } catch (cause) {
    error = redact(cause?.message ?? cause)
  }
  error ??= provider.lastError
  const terminal = [...recorder.events].reverse().find(event => event.kind === "run_terminal")
  if (!error && terminal?.reason && terminal.reason !== "completed") error = `termination: ${terminal.reason}`
  if (process.env.LEDGER_DEBUG) {
    const { writeFile } = await import("node:fs/promises")
    await writeFile(process.env.LEDGER_DEBUG, JSON.stringify({ events: recorder.events, lastContext: provider.lastContext }, null, 1))
  }
  const success = answer.replace(/,/g, "").includes(expected)
  return trialFrom({ scenario: "ledger-audit", variant, task: `${account}#${index}`, success, error, provider, events: recorder.events })
}

const trials = []
for (let index = 0; index < taskCount; index++) {
  const trial = await runTask(index)
  trials.push(trial)
  console.error(`${trial.task}: ${trial.success ? "correct" : "wrong"} ${trial.cost.calls} calls ${trial.cost.sent_chars} chars${trial.error ? ` (${trial.error})` : ""}`)
}
await writeTrials(out, trials)
console.error(`${variant} on ${model}: ${trials.filter(trial => trial.success).length}/${trials.length} correct`)
