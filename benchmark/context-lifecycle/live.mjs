#!/usr/bin/env node
// Live check for plan task 3.1: does a short dependency preview cost answer quality?
//
//   node benchmark/context-lifecycle/live.mjs [--repo <checkout>] [--mode full|handle] [--tasks N]
//                                             [--provider openai|deepseek]
//
// Three explore nodes return fixed 16k-character survey reports without calling a model; only the
// synthesize node runs on the real provider from `.env` (OPENAI_* or DEEPSEEK_*). Each task asks for one access
// code that sits inside the 600-character preview, inside the 8,000-character clip, or beyond it.
// The answer is graded by exact match. Prints JSON; never prints a key.

import { readFile } from "node:fs/promises"
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

const args = process.argv.slice(2)
const arg = (name, fallback) => (args.includes(name) ? args[args.indexOf(name) + 1] : fallback)
const here = new URL("../..", import.meta.url).pathname
const repo = resolve(arg("--repo", here))
const mode = arg("--mode", "full")
const taskCount = Number(arg("--tasks", "10"))
const providerName = arg("--provider", "openai")

for (const line of (await readFile(resolve(here, ".env"), "utf8")).split(/\r?\n/)) {
  const match = line.trim().match(/^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/)
  if (match && !(match[1] in process.env)) process.env[match[1]] = match[2].trim().replace(/^("|')(.*)\1$/, "$2")
}

const dist = file => import(pathToFileURL(resolve(repo, "node/dist", file)).href)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const { defaultSubAgentOrchestrator } = await dist("workflow/public.js")

const retry = { maxRetries: 2, baseDelay: 1000 }
const model = providerName === "deepseek" ? process.env.DEEPSEEK_MODEL ?? "deepseek-chat" : process.env.OPENAI_MODEL ?? "gpt-4o-mini"
const inner = providerName === "deepseek"
  ? (await dist("providers/public.js")).deepseek({
    apiKey: process.env.DEEPSEEK_API_KEY,
    model,
    ...(process.env.DEEPSEEK_BASE_URL ? { baseURL: process.env.DEEPSEEK_BASE_URL } : {}),
    retry,
  })
  : new root.OpenAIProvider({
    apiKey: process.env.OPENAI_API_KEY,
    model,
    ...(process.env.OPENAI_BASE_URL ? { baseURL: process.env.OPENAI_BASE_URL } : {}),
    retry,
  })
const redact = text => String(text).replace(/Bearer\s+\S+/gi, "Bearer [REDACTED]").replace(/sk-[A-Za-z0-9_-]{8,}/g, "sk-[REDACTED]").slice(0, 240)

// The proxy does not report usage reliably, so the cost measure is what each call sends.
// Provider failures are recorded here, since the workflow turns them into an empty answer.
let sentChars = 0
let calls = 0
let lastProviderError
const sent = context => JSON.stringify([context.systemStable ?? context.systemText ?? "", context.systemKnowledge ?? "", context.turns ?? [], context.stateTurn ?? ""]).length
const provider = {
  async complete(context, ...rest) {
    sentChars += sent(context); calls += 1
    try { return await inner.complete(context, ...rest) } catch (cause) { lastProviderError = redact(cause?.message ?? cause); throw cause }
  },
  async *stream(context, ...rest) {
    sentChars += sent(context); calls += 1
    try { yield* inner.stream(context, ...rest) } catch (cause) { lastProviderError = redact(cause?.message ?? cause); throw cause }
  },
}

// ── fixtures ───────────────────────────────────────────────────────────────────────────────────

const ROWS = 200
const code = (area, row) => `${String.fromCharCode(65 + ((area * 7 + row) % 26))}${String.fromCharCode(65 + ((row * 3 + area) % 26))}-${String(1000 + ((area * 7919 + row * 104729) % 9000)).padStart(4, "0")}`
const report = area => `Survey report for area-${area}.\n` + Array.from({ length: ROWS }, (_, row) =>
  `station ${area}-${String(row).padStart(3, "0")}: status nominal, access code ${code(area, row)}, last check day ${row % 28 + 1}`,
).join("\n")

// Rows 5 sit inside a 600-char preview; rows 40 inside the 8,000-char clip; rows 150 beyond it.
const DEPTHS = [5, 40, 150]
const TASKS = Array.from({ length: taskCount }, (_, i) => {
  const area = (i % 3) + 1
  const row = DEPTHS[i % DEPTHS.length]
  return { id: `t${i}`, area, row, depth: row === 5 ? "preview" : row === 40 ? "clip" : "beyond", expected: code(area, row) }
})

// ── one task ───────────────────────────────────────────────────────────────────────────────────

async function runTask(task) {
  const charsBefore = sentChars
  const callsBefore = calls
  lastProviderError = undefined
  let answer = ""
  let termination
  const orchestrator = {
    async run(ctx) {
      const area = /survey area-(\d)/.exec(ctx.spec.goal)?.[1]
      if (area && !ctx.spec.goal.includes("Answer")) {
        return {
          agentId: ctx.spec.identity.agentId,
          result: { termination: "completed", finalMessage: { role: "assistant", content: report(Number(area)), toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 0 },
        }
      }
      const result = await defaultSubAgentOrchestrator.run(ctx)
      const content = result.result.finalMessage?.content
      answer = typeof content === "string" ? content : JSON.stringify(content ?? "")
      termination = result.result.termination
      return result
    },
  }
  const runner = new RuntimeRunner({
    provider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 64_000,
    maxTurns: 6,
    maxTotalTokens: 60_000,
    subAgentOrchestrator: orchestrator,
  })
  const question = `Answer from the three survey reports: what is the access code of station ${task.area}-${String(task.row).padStart(3, "0")}? Reply with the code only.`
  const started = Date.now()
  let error
  try {
    await runner.runWorkflow({
      nodes: [
        { task: "survey area-1", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: "survey area-2", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: "survey area-3", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: question, role: "plan", dependsOn: [0, 1, 2], ...(mode === "handle" ? { context: { dependencyMode: "handle" } } : {}) },
      ],
    }, { sessionId: `live-${task.id}` })
  } catch (cause) {
    error = redact(cause?.message ?? cause)
  }
  error ??= lastProviderError ?? (termination && termination !== "completed" ? `termination: ${termination}` : undefined)
  return {
    id: task.id,
    depth: task.depth,
    correct: answer.includes(task.expected),
    answer: answer.slice(0, 160),
    sentChars: sentChars - charsBefore,
    calls: calls - callsBefore,
    ms: Date.now() - started,
    ...(error ? { error } : {}),
  }
}

const results = []
for (const task of TASKS) results.push(await runTask(task))
const byDepth = {}
for (const result of results) {
  const bucket = (byDepth[result.depth] ??= { correct: 0, total: 0, sentChars: 0 })
  bucket.total += 1
  bucket.sentChars += result.sentChars
  if (result.correct) bucket.correct += 1
}
console.log(JSON.stringify({
  repo,
  mode,
  provider: providerName,
  model,
  correct: results.filter(result => result.correct).length,
  total: results.length,
  sentChars: results.reduce((sum, result) => sum + result.sentChars, 0),
  calls: results.reduce((sum, result) => sum + result.calls, 0),
  byDepth,
  results,
}, null, 2))
