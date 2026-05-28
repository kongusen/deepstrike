#!/usr/bin/env node
/**
 * P0 live validation against a real LLM (reads ../.env).
 * Verifies: multi-turn tool loop, token usage reporting, no premature compression.
 */
import { readFileSync } from "node:fs"
import {
  RuntimeRunner,
  OpenAIChatProvider,
  tool,
  LocalExecutionPlane,
  InMemorySessionLog,
} from "../node/dist/index.js"

loadEnvFile(new URL("../.env", import.meta.url))

const apiKey = process.env.OPENAI_API_KEY
const baseURL = process.env.OPENAI_BASE_URL
const model = process.env.OPENAI_MODEL ?? "gpt-4o-mini"

if (!apiKey) {
  console.error("OPENAI_API_KEY missing in .env")
  process.exit(1)
}

const provider = new OpenAIChatProvider(apiKey, model, { maxRetries: 2, baseDelay: 1000 }, baseURL)
const sessionLog = new InMemorySessionLog()
const plane = new LocalExecutionPlane()

let turnCounter = 0
plane.register(tool("note_turn", "Record the current turn number and return OK.", {
  type: "object",
  properties: { n: { type: "number", description: "turn number" } },
  required: ["n"],
}, async (args) => {
  turnCounter += 1
  return JSON.stringify({ ok: true, recorded: args.n, server_turn: turnCounter })
}))

const runner = new RuntimeRunner({
  provider,
  sessionLog,
  executionPlane: plane,
  maxTokens: 8192,
  maxTurns: 6,
  systemPrompt: "You are a test assistant. Call note_turn once, then reply DONE.",
})

const sessionId = `p0-live-${Date.now()}`
const usageRounds = []
const errors = []
let done = null
const t0 = performance.now()
const RUN_TIMEOUT_MS = 120_000

try {
  const runIter = runner.run({
    sessionId,
    goal: "Call note_turn with n=1 once, then reply exactly: DONE",
    criteria: ["Calls note_turn once", "Final message contains DONE"],
  })
  const timeout = new Promise((_, rej) =>
    setTimeout(() => rej(new Error(`run timeout after ${RUN_TIMEOUT_MS}ms`)), RUN_TIMEOUT_MS),
  )
  await Promise.race([
    (async () => {
      for await (const evt of runIter) {
        if (evt.type === "usage") {
          usageRounds.push({
            inputTokens: evt.inputTokens,
            outputTokens: evt.outputTokens,
            totalTokens: evt.totalTokens,
          })
        } else if (evt.type === "error") {
          errors.push(evt.message)
        } else if (evt.type === "done") {
          done = evt
        }
      }
    })(),
    timeout,
  ])
} catch (err) {
  errors.push(String(err))
}

const elapsedMs = Math.round(performance.now() - t0)
const log = await sessionLog.read(sessionId)

const compressed = log.filter(e => e.event.kind === "compressed")
const llmCompleted = log.filter(e => e.event.kind === "llm_completed")
const toolRequested = log.filter(e => e.event.kind === "tool_requested")

const assistantTokenCounts = llmCompleted.map(e => e.event.token_count).filter(n => n != null)

const report = {
  model,
  baseURL: baseURL ?? "(default)",
  elapsed_ms: elapsedMs,
  status: done?.status ?? "error",
  iterations: done?.iterations ?? 0,
  total_tokens_reported: done?.totalTokens ?? 0,
  tool_rounds: toolRequested.length,
  usage_rounds: usageRounds,
  compressed_events: compressed.length,
  assistant_token_counts: assistantTokenCounts,
  assistant_counts_look_like_output_only: assistantTokenCounts.every(t => t < 2000),
  errors,
  pass: errors.length === 0
    && compressed.length === 0
    && (done?.status === "completed" || done?.status === "max_turns")
    && toolRequested.length >= 2,
}

console.log(JSON.stringify(report, null, 2))
process.exit(report.pass ? 0 : 1)

function loadEnvFile(url) {
  try {
    const raw = readFileSync(url, "utf8")
    for (const line of raw.split(/\r?\n/)) {
      const trimmed = line.trim()
      if (!trimmed || trimmed.startsWith("#")) continue
      const index = trimmed.indexOf("=")
      if (index === -1) continue
      const key = trimmed.slice(0, index)
      const value = trimmed.slice(index + 1)
      if (!(key in process.env)) process.env[key] = value
    }
  } catch {
    // optional
  }
}
