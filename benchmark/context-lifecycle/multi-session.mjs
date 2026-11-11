#!/usr/bin/env node
// Live multi-session benchmark for plan task 9.4: what a model learns in one session is only worth
// something if a later session can use it.
//
//   node benchmark/context-lifecycle/multi-session.mjs [--repo <checkout>] [--variant <name>]
//        [--memory none|notes|consolidate] [--sequences N] [--sessions N] [--start N]
//        [--concurrency N] [--provider deepseek|openai|ollama|mock] [--max-tokens N] [--dry]
//        [--out trials.jsonl]
//
// A sequence is `--sessions` (default 8) consecutive sessions in one domain. Each session asks for
// the number of ERROR lines with one code across six service logs and accepts one answer through
// `submit`. Two rules of the domain are never written down — say, "a request id ending in 0 is a
// client retry and does not count", "lines from host=canary do not count" — so the first sessions
// fail. A wrong `submit` says what was expected and shows a few lines that should not have counted:
// enough for a model to work the rule out, and to remember it, if it can.
//
// Memory (`--memory`):
//   none         each session starts from nothing (the baseline)
//   notes        notes on, and a memory store the model can propose records to with
//                `propose_memory` (the host accepts every proposal); later sessions recall them
//   consolidate  as notes, plus a consolidation run after every session (an ordinary run with
//                `transcript_search`), whose cost counts toward the session it follows
//
// One trial per session (`task` = `SEQ#<sequence>@<session>`), so sessions pair across variants.
// Oracles: `memory_records` (records in the store when the session starts), `proposals`
// (`propose_memory` calls in the session), `memory_reads` (`memory` calls). Never prints a key.

import { resolve } from "node:path"
import { MeteredProvider, here, loadEnv, parseArgs, realProvider, recordingSessionLog, redact, sdk, trialFrom, writeTrials } from "./trial.mjs"

const arg = parseArgs()
const repo = resolve(arg("--repo", here))
const memoryMode = arg("--memory", "none")
const variant = arg("--variant", `multi-${memoryMode}`)
const sequences = Number(arg("--sequences", "6"))
const sessions = Number(arg("--sessions", "8"))
const start = Number(arg("--start", "0"))
const concurrency = Math.max(1, Number(arg("--concurrency", "3")))
const maxTokens = Number(arg("--max-tokens", "32000"))
const providerName = arg("--provider", "deepseek")
const out = arg("--out")
if (!["none", "notes", "consolidate"].includes(memoryMode)) throw new Error(`--memory must be none, notes or consolidate`)

// ── fixtures (deterministic per sequence and session) ──────────────────────────────────────────

function rng(seed) {
  let state = (seed * 2654435761) >>> 0 || 1
  return () => {
    state ^= state << 13; state >>>= 0
    state ^= state >>> 17
    state ^= state << 5; state >>>= 0
    return state / 4294967296
  }
}

/** The domain's unwritten rules. A line a rule `drops` does not count. */
const RULES = [
  { id: "retry", drops: line => line.req.endsWith("0") },
  { id: "canary", drops: line => line.host === "canary" },
  { id: "replayed", drops: line => line.replayed },
  { id: "synthetic", drops: line => line.user === "synthetic-probe" },
]
const CODES = ["E401", "E403", "E409", "E429", "E500", "E502", "E503", "E504"]
const LEVELS = ["INFO", "INFO", "WARN", "DEBUG", "ERROR", "ERROR"]
const HOSTS = ["web-1", "web-2", "web-3", "canary"]
const USERS = ["u-1042", "u-2210", "u-3307", "synthetic-probe"]
const LOGS = 6
const LINES = 40

/** A sequence's two rules: the same for every session of the sequence. */
function sequenceRules(sequence) {
  const next = rng(1000 + sequence)
  const first = Math.floor(next() * RULES.length)
  const second = (first + 1 + Math.floor(next() * (RULES.length - 1))) % RULES.length
  return [RULES[first], RULES[second]]
}

function session(sequence, index) {
  const next = rng(sequence * 101 + index + 7)
  const pick = list => list[Math.floor(next() * list.length)]
  const rules = sequenceRules(sequence)
  const code = pick(CODES)
  const logs = Array.from({ length: LOGS }, (_, file) => Array.from({ length: LINES }, (_, n) => {
    const level = pick(LEVELS)
    const line = {
      level,
      code: level !== "ERROR" ? "-" : next() < 0.18 ? code : pick(CODES.filter(c => c !== code)),
      req: `r${String(Math.floor(next() * 1e6)).padStart(6, "0")}`,
      host: pick(HOSTS),
      user: pick(USERS),
      replayed: next() < 0.15,
    }
    const time = `2026-11-${String(1 + index).padStart(2, "0")} ${String(n % 24).padStart(2, "0")}:${String((n * 7) % 60).padStart(2, "0")}`
    line.text = `${time} ${level.padEnd(5)} code=${line.code} req=${line.req} host=${line.host} user=${line.user}${line.replayed ? " [replayed]" : ""} svc=svc-${file + 1}`
    return line
  }))
  const matching = logs.flat().filter(line => line.level === "ERROR" && line.code === code)
  const counted = matching.filter(line => !rules.some(rule => rule.drops(line)))
  const dropped = matching.filter(line => rules.some(rule => rule.drops(line)))
  return {
    code, rules: rules.map(rule => rule.id), expected: counted.length, dropped,
    files: new Map(logs.map((lines, file) => [`logs/svc-${file + 1}.log`, lines.map(line => line.text).join("\n")])),
    goal: `Session ${index + 1} of the incident desk. Count the log lines with level ERROR and code=${code} across logs/ (list_files, read_file). Some lines do not count by this team's rules, which are not written down anywhere; earlier sessions may have learned them. Submit the count once with submit(count).`,
  }
}

if (process.argv.includes("--dry")) {
  for (let s = start; s < start + sequences; s++) {
    for (let k = 0; k < sessions; k++) {
      const t = session(s, k)
      console.log(`SEQ#${s}@${k}: code=${t.code} rules=${t.rules.join(",")} expected=${t.expected} dropped=${t.dropped.length}`)
    }
  }
  process.exit(0)
}

await loadEnv()
const dist = sdk(repo)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const memoryApi = await dist("memory/public.js")

/** `--provider mock`: a scripted model that lists, reads, writes a note, proposes a rule after a
 *  failure, and submits. Checks the plumbing without a provider account. */
function mockProvider() {
  return {
    model: "mock",
    provider: {
      async complete() { return { role: "assistant", content: "", toolCalls: [] } },
      async *stream(context, tools) {
        const names = tools.map(tool => tool.name)
        const said = JSON.stringify(context.turns ?? [])
        const results = (said.match(/"role":"tool"/g) ?? []).length
        if (results === 0) { yield { type: "tool_call", id: "m1", name: "list_files", arguments: {} }; return }
        if (results === 1) { yield { type: "tool_call", id: "m2", name: "read_file", arguments: { path: "logs/svc-1.log" } }; return }
        if (results === 2 && names.includes("note")) { yield { type: "tool_call", id: "m3", name: "note", arguments: { title: "Tally", body: "svc-1 read" } }; return }
        if (!said.includes("submitted")) { yield { type: "tool_call", id: "m4", name: "submit", arguments: { count: 1 } }; return }
        if (said.includes("incorrect") && names.includes("propose_memory") && !said.includes("m5")) {
          yield { type: "tool_call", id: "m5", name: "propose_memory", arguments: { name: "unwritten-rule", kind: "project", content: "Some ERROR lines do not count; see the lines a wrong submit showed." } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    },
  }
}

const { provider: inner, model } = providerName === "mock" ? mockProvider() : await realProvider(dist, providerName)

/** A host that accepts every proposal straight into the store, so later sessions can recall it. */
function acceptingSink(store) {
  const sink = {
    accepted: 0,
    async submit(agentId, candidate) {
      await store.put(agentId, candidate.record)
      sink.accepted += 1
      const n = sink.accepted
      return { status: "accepted", candidate_id: candidate.candidate_id, receipt_id: `r-${n}`, record_ref: candidate.record.record_id, digest: `d-${n}` }
    },
  }
  return sink
}

// ── one sequence ───────────────────────────────────────────────────────────────────────────────

async function runSequence(sequence) {
  const trials = []
  const agentId = `incident-desk-${sequence}`
  const scope = { tenant_id: agentId, namespace: "incidents" }
  const store = new memoryApi.InMemoryMemoryStore()
  const sink = acceptingSink(store)
  const transcripts = new InMemorySessionLog()
  const done = []
  for (let k = 0; k < sessions; k++) {
    const t = session(sequence, k)
    const provider = new MeteredProvider(inner)
    const recorder = recordingSessionLog(transcripts)
    const plane = new LocalExecutionPlane()
    let submitted, proposals = 0, memoryReads = 0
    plane.register(root.tool("list_files", "List the log files.", { type: "object", properties: {} }, async () => [...t.files.keys()].join("\n")))
    plane.register(root.tool("read_file", "Read one log file by its path.",
      { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
      async ({ path }) => t.files.get(String(path)) ?? `no such file: ${path}`))
    plane.register(root.tool("submit", "Submit the count. Only the first submission is graded.",
      { type: "object", properties: { count: { type: "integer" } }, required: ["count"] },
      async ({ count }) => {
        if (submitted !== undefined) return "already submitted; the session is over"
        submitted = Number(count)
        if (submitted === t.expected) return "submitted: correct"
        const examples = t.dropped.slice(0, 3).map(line => `  ${line.text}`).join("\n")
        return `submitted: incorrect, the expected count was ${t.expected}. Lines like these match the code but do not count:\n${examples}`
      }))
    const memory = memoryMode === "none" ? {} : {
      agentId, memoryScope: scope, memoryStore: store, memoryCandidateSink: sink,
      memoryRuntimePolicy: { retrieval: "host", modelWrites: "candidate_sink" },
      contextPolicy: { notesBudgetTokens: 600 },
    }
    const recordsAtStart = sink.accepted
    const sessionId = `seq-${sequence}-s${k}`
    let error
    try {
      const runner = new RuntimeRunner({
        provider, sessionLog: recorder.log, executionPlane: plane, maxTokens, maxTurns: 40,
        baselineToolIds: ["list_files", "read_file", "submit"], ...memory,
      })
      for await (const event of runner.run({ sessionId, goal: t.goal })) {
        if (event.type === "tool_call" && event.name === "propose_memory") proposals += 1
        if (event.type === "tool_call" && event.name === "memory") memoryReads += 1
        if (event.type === "error") error ??= redact(`run error: ${event.message}`)
      }
      done.push(sessionId)
      if (memoryMode === "consolidate") {
        await memoryApi.consolidateMemory({
          provider, sessionLog: recorder.log, sessionIds: [...done], sessionId: `${sessionId}-consolidate`,
          maxTokens, maxTurns: 20, ...memory,
        })
      }
    } catch (cause) {
      error = redact(cause?.message ?? cause)
    }
    error ??= provider.lastError
    const trial = trialFrom({
      scenario: "multi-session", variant, task: `SEQ#${sequence}@${k}`, success: submitted === t.expected,
      error, provider, events: recorder.events, model,
    })
    trials.push({
      ...trial,
      oracle: { memory_records: recordsAtStart, proposals, memory_reads: memoryReads, submitted: submitted === undefined ? 0 : 1 },
      expected: String(t.expected),
      answer: submitted === undefined ? "" : String(submitted),
    })
    await writeTrials(out, [trials.at(-1)])
    console.error(`SEQ#${sequence}@${k}: ${trials.at(-1).success ? "correct" : "wrong"} (expected ${t.expected}, got ${submitted ?? "-"}) records ${recordsAtStart} proposals ${proposals}${error ? ` (${error})` : ""}`)
  }
  return trials
}

const queue = Array.from({ length: sequences }, (_, i) => start + i)
const all = []
async function worker() {
  for (let s = queue.shift(); s !== undefined; s = queue.shift()) all.push(...await runSequence(s))
}
await Promise.all(Array.from({ length: concurrency }, worker))
for (let k = 0; k < sessions; k++) {
  const at = all.filter(trial => trial.task.endsWith(`@${k}`))
  console.error(`session ${k + 1}: ${at.filter(trial => trial.success).length}/${at.length} correct`)
}
console.error(`${variant} on ${model}: ${all.filter(trial => trial.success).length}/${all.length} correct`)
