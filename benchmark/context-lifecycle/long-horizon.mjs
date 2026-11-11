#!/usr/bin/env node
// Live long-horizon task for plan task 8.2: an incident investigation far longer than its window.
//
//   node benchmark/context-lifecycle/long-horizon.mjs [--repo <checkout>] [--variant <name>]
//        [--policy '<ContextPolicy overrides JSON>'] [--tasks N] [--start N] [--concurrency N]
//        [--logs N] [--lines N] [--provider deepseek|openai] [--max-tokens N] [--model-summaries]
//        [--out trials.jsonl]
//
// The model first reads `brief.md`, which holds the scoring rules and nothing else does: the error
// code to count, request ids to skip, and — needed only for the final answer — a multiplier and an
// on-call badge number to add. Then it reads `--logs` (default 40) service logs, one per call, each a
// few thousand tokens, so the logs alone overflow the window (`--max-tokens`, default 100000) and
// the run must compact or renew long after the brief was read. The answer is
// `count × multiplier + badge`, graded by exact match.
//
// Oracles (lower is better): `formula_broken` is 1 when the final number does not have the brief's
// form `n × multiplier + badge` (an early fact was lost or misapplied); otherwise `count_error` is
// |n − true count| and `undercount` is 1 when n fell short — the run lost tallies it had made, which
// no file can give back (unlike the brief, which the model can read again). `early_fact_lost` is 1 when the last request the model answered from
// no longer showed the badge number anywhere (history, task state or knowledge) — the brief's
// end-only fact did not survive; `brief_rereads` counts reads of the brief after the first;
// `compactions` and `renewals` count the kernel's compaction and renewal events, so a run that never
// reached them can be told apart. Writes one trial per task for `deepstrike-lab`; never prints a key.

import { resolve } from "node:path"
import { MeteredProvider, here, loadEnv, parseArgs, realProvider, recordingSessionLog, redact, sdk, trialFrom, writeTrials } from "./trial.mjs"

const arg = parseArgs()
const repo = resolve(arg("--repo", here))
const variant = arg("--variant", "default")
const policy = JSON.parse(arg("--policy", "{}"))
const taskCount = Number(arg("--tasks", "30"))
const start = Number(arg("--start", "0"))
const concurrency = Math.max(1, Number(arg("--concurrency", "5")))
const maxTokens = Number(arg("--max-tokens", "100000"))
const LOGS = Number(arg("--logs", "40"))
const LINES = Number(arg("--lines", "110"))
const out = arg("--out")
// Plan task 8.4 · `--model-summaries`: range compactions ask the run's model for their summary.
const modelSummaries = process.argv.includes("--model-summaries")

await loadEnv()
const dist = sdk(repo)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const { provider: inner, model } = await realProvider(dist, arg("--provider", "deepseek"))

// ── fixtures (deterministic per task) ──────────────────────────────────────────────────────────

/** A small deterministic generator, so every variant sees the same task. */
function rng(seed) {
  let state = (seed * 2654435761) >>> 0 || 1
  return () => {
    state ^= state << 13; state >>>= 0
    state ^= state >>> 17
    state ^= state << 5; state >>>= 0
    return state / 4294967296
  }
}

const LEVELS = ["INFO", "INFO", "INFO", "WARN", "DEBUG", "ERROR", "INFO", "ERROR"]
const CODES = ["E401", "E403", "E404", "E409", "E429", "E500", "E502", "E503", "E504"]
const MESSAGES = [
  "upstream call returned after retry budget was partly consumed",
  "cache miss on tenant configuration, falling back to the primary store",
  "request completed with degraded latency on the payment path",
  "connection pool saturated; queued behind long-running export jobs",
  "token refresh raced with a concurrent logout from the same session",
  "schema validation rejected an optional field sent by an older client",
]

function task(index) {
  const next = rng(index + 1)
  const pick = list => list[Math.floor(next() * list.length)]
  const code = pick(CODES)
  const multiplier = 2 + Math.floor(next() * 8)
  const badge = 1000 + Math.floor(next() * 9000)
  const logs = Array.from({ length: LOGS }, (_, file) => {
    const lines = Array.from({ length: LINES }, (_, line) => {
      const level = pick(LEVELS)
      // The brief's code is rare (about one match per log), so the answer turns on keeping the
      // brief's rules rather than on counting hundreds of lines.
      const lineCode = level !== "ERROR" ? "-" : next() < 0.012 ? code : pick(CODES.filter(c => c !== code))
      const req = `r${String(Math.floor(next() * 1e6)).padStart(6, "0")}`
      const time = `2026-10-${String(1 + (file % 28)).padStart(2, "0")} ${String(line % 24).padStart(2, "0")}:${String((line * 7) % 60).padStart(2, "0")}:${String((line * 13) % 60).padStart(2, "0")}`
      return { text: `${time} ${level.padEnd(5)} code=${lineCode} req=${req} svc=svc-${String(file + 1).padStart(2, "0")} msg="${pick(MESSAGES)}"`, level, code: lineCode, req }
    })
    return lines
  })
  let count = 0
  for (const lines of logs) {
    for (const line of lines) {
      if (line.level === "ERROR" && line.code === code && !line.req.endsWith("0")) count += 1
    }
  }
  const brief = [
    "# Incident brief INC-7731",
    "",
    "Scoring rules for this investigation. They are written down only here.",
    "",
    `1. Count the log lines with level ERROR and code=${code}.`,
    "2. Skip any such line whose request id (req=...) ends in the digit 0.",
    `3. The incident score is (that count × ${multiplier}) + ${badge}, where ${badge} is the on-call engineer's badge number.`,
    "",
    "Background: the incident window covers the whole of October; all services in logs/ are in scope.",
  ].join("\n")
  return {
    code, multiplier, badge, count,
    expected: String(count * multiplier + badge),
    files: new Map([["brief.md", brief], ...logs.map((lines, file) => [`logs/svc-${String(file + 1).padStart(2, "0")}.log`, lines.map(line => line.text).join("\n")])]),
  }
}

/** The answer-derived oracles: did the final number keep the brief's form, and how far was its count. */
function answerOracles(t, answer) {
  const numbers = answer.replace(/,/g, "").match(/\d+/g) ?? []
  const last = numbers.length ? Number(numbers[numbers.length - 1]) : NaN
  const implied = (last - t.badge) / t.multiplier
  const formed = Number.isInteger(implied) && implied >= 0
  return {
    formula_broken: formed ? 0 : 1,
    count_error: formed ? Math.abs(implied - t.count) : t.count,
    undercount: formed && implied < t.count ? 1 : 0,
  }
}

// `--rescore <file>`: recompute the answer-derived oracles of recorded long-horizon trials in place
// (they depend only on the recorded answer and the task's facts).
if (process.argv.includes("--rescore")) {
  const { readFile, writeFile } = await import("node:fs/promises")
  const file = resolve(arg("--rescore"))
  const lines = (await readFile(file, "utf8")).split("\n").filter(line => line.trim())
  const rescored = lines.map(line => {
    const trial = JSON.parse(line)
    if (trial.scenario !== "long-horizon") return line
    const t = task(Number(trial.task.split("#")[1]))
    return JSON.stringify({ ...trial, oracle: { ...trial.oracle, ...answerOracles(t, trial.answer ?? "") } })
  })
  await writeFile(file, rescored.join("\n") + "\n")
  console.error(`rescored ${rescored.length} trials`)
  process.exit(0)
}

// `--dry`: print each task's facts and sizes, call no model.
if (process.argv.includes("--dry")) {
  for (let index = start; index < start + taskCount; index++) {
    const t = task(index)
    const chars = [...t.files.values()].map(text => text.length)
    console.log(`INC#${index}: code ${t.code} ×${t.multiplier} +${t.badge} count ${t.count} → ${t.expected}; ${chars.length} files, log ~${Math.round(chars[1] / 4)} tokens each, ~${Math.round(chars.reduce((a, b) => a + b, 0) / 4)} tokens in all`)
  }
  process.exit(0)
}

// ── one task ───────────────────────────────────────────────────────────────────────────────────

async function runTask(index) {
  const t = task(index)
  const provider = new MeteredProvider(inner)
  const recorder = recordingSessionLog(new InMemorySessionLog())
  const plane = new LocalExecutionPlane()
  let briefReads = 0
  plane.register(root.tool(
    "list_files",
    "List the files of the investigation.",
    { type: "object", properties: {} },
    async () => [...t.files.keys()].join("\n"),
  ))
  plane.register(root.tool(
    "read_file",
    "Read one file of the investigation by its path.",
    { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
    async ({ path }) => {
      if (path === "brief.md") briefReads += 1
      return t.files.get(String(path)) ?? `no such file: ${path}`
    },
  ))
  const runner = new RuntimeRunner({
    provider,
    sessionLog: recorder.log,
    executionPlane: plane,
    maxTokens,
    maxTurns: 90,
    maxTotalTokens: 8_000_000,
    baselineToolIds: ["list_files", "read_file"],
    enablePlanTool: true,
    agentId: "incident-investigator",
    ...(Object.keys(policy).length ? { contextPolicy: policy } : {}),
    ...(modelSummaries ? { modelSummaries: true } : {}),
  })
  const goal = "Investigate incident INC-7731. Read brief.md first: it defines how to score the incident. Then list the files and read every log under logs/, one file per read_file call, applying the brief's rules to each. Reply with the final incident score only, as an integer."
  let answer = ""
  let error
  try {
    for await (const event of runner.run({ sessionId: `horizon-${index}`, goal })) {
      if (event.type === "text_delta") answer += event.delta
    }
  } catch (cause) {
    error = redact(cause?.message ?? cause)
  }
  error ??= provider.lastError
  const terminal = [...recorder.events].reverse().find(event => event.kind === "run_terminal")
  if (!error && terminal?.reason && terminal.reason !== "completed") error = `termination: ${terminal.reason}`
  const lastSeen = JSON.stringify(provider.lastContext ?? {})
  const final = answer.replace(/[,\s]/g, "")
  const trial = trialFrom({
    scenario: "long-horizon", variant, task: `INC#${index}`, success: final.includes(t.expected),
    error, provider, events: recorder.events, model,
  })
  return {
    ...trial,
    oracle: {
      early_fact_lost: lastSeen.includes(String(t.badge)) ? 0 : 1,
      brief_rereads: Math.max(0, briefReads - 1),
      compactions: recorder.events.filter(event => event.kind === "compressed").length,
      renewals: recorder.events.filter(event => event.kind === "context_renewed").length,
      // Plan task 8.4 · summary calls, and those that fell back to the rule summary.
      model_summaries: recorder.events.filter(event => event.kind === "context_summarized" && event.status === "success").length,
      summary_fallbacks: recorder.events.filter(event => event.kind === "context_summarized" && event.status !== "success").length,
      ...answerOracles(t, answer),
    },
    expected: t.expected,
    // The tail of the reply: the final number is in it, and `--rescore` reads it back.
    answer: answer.slice(-200),
  }
}

// Tasks run `--concurrency` at a time; each trial is written as soon as it finishes, so an
// interrupted run keeps what it measured.
const indices = Array.from({ length: taskCount }, (_, i) => start + i)
const trials = []
async function worker() {
  for (let index = indices.shift(); index !== undefined; index = indices.shift()) {
    const trial = await runTask(index)
    trials.push(trial)
    await writeTrials(out, [trial])
    console.error(`${trial.task}: ${trial.success ? "correct" : "wrong"} ${trial.cost.calls} calls, ${trial.cost.input_tokens} input tokens, early_fact_lost ${trial.oracle.early_fact_lost}, compactions ${trial.oracle.compactions}, renewals ${trial.oracle.renewals}${trial.error ? ` (${trial.error})` : ""}`)
  }
}
await Promise.all(Array.from({ length: concurrency }, worker))
console.error(`${variant} on ${model}: ${trials.filter(trial => trial.success).length}/${trials.length} correct`)
