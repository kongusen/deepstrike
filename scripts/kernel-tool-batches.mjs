#!/usr/bin/env node
// Offline check of turns that mix kernel tools (`memory`, `propose_memory`, `note`) with host
// tools: a scripted provider plays fixed or random batches against the built Node SDK, and after
// every request each assistant tool call in the rendered turns must have its result before any
// other message (the rule OpenAI-compatible providers enforce). No provider account needed.
//
//   node scripts/kernel-tool-batches.mjs                 the fixed cases
//   ONLY="mem+prop" DUMP=1 node scripts/kernel-tool-batches.mjs   one case, with its last turns
//   BIG=1 FUZZ=7 RUNS=40 node scripts/kernel-tool-batches.mjs     random batches, seeded
//
// Rebuild the napi addon and `node/dist` first. Prints `ok` / `FAIL` per case and exits 1 on any
// FAIL. Known failures are listed in docs/plans/kernel-distillation.md, task 4.4.
import { resolve } from "node:path"
import { pathToFileURL } from "node:url"
const repo = new URL("..", import.meta.url).pathname
const dist = f => import(pathToFileURL(resolve(repo, "node/dist", f)).href)
const root = await dist("index.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const memoryApi = await dist("memory/public.js")

function unanswered(turns) {
  // Same rule as the OpenAI-chat adapter: every call answered before any non-tool message.
  const bad = []
  let pending = null, done = new Set()
  const flush = why => { if (pending) for (const id of pending) if (!done.has(id)) bad.push(why + ":" + id) }
  for (const m of turns) {
    if (m.role === "assistant") { flush("next-assistant"); pending = m.toolCalls?.length ? m.toolCalls.map(c => c.id) : null; done = new Set() }
    else if (m.role === "tool") { for (const b of m.contentParts ?? m.blocks ?? []) { if (b.type === "tool_result") done.add(b.callId) } }
    else { flush(m.role + "-before-result"); pending = null; done = new Set() }
  }
  flush("end")
  return bad
}

const C = {
  mem: q => ({ name: "memory", arguments: { query: q ?? "rules" } }),
  fetch: id => ({ name: "memory", arguments: { record_id: id } }),
  prop: n => ({ name: "propose_memory", arguments: { name: n ?? "rule-a", kind: "project", content: "canary lines do not count", description: "counting rule" } }),
  note: t => ({ name: "note", arguments: { title: t ?? "Tally", body: "x" } }),
  read: () => ({ name: "read_file", arguments: { path: "a" } }),
  plan: () => ({ name: "update_plan", arguments: { plan: ["a"], current_step: 0 } }),
}

async function scenario(label, script, { seed = true, slowSink = 0, slowStore = 0 } = {}) {
  const store = new memoryApi.InMemoryMemoryStore()
  const agentId = "a", scope = { tenant_id: "a", namespace: "n" }
  let n = 0
  const sink = { async submit(agent, cand) { if (slowSink) await new Promise(r => setTimeout(r, slowSink)); await store.put(agent, cand.record); n++; return { status: "accepted", candidate_id: cand.candidate_id, receipt_id: `r${n}`, record_ref: cand.record.record_id, digest: `d${n}` } } }
  const plane = new LocalExecutionPlane()
  plane.register(root.tool("read_file", "read", { type: "object", properties: { path: { type: "string" } } }, async ({ path }) => { await new Promise(r => setTimeout(r, 2)); return process.env.BIG ? Array.from({ length: 40 }, (_, n) => `2026-11-01 ${n} ERROR code=E50${n % 9} req=r${(n * 7919 + String(path).length * 31) % 1000000} host=web-${n % 3} user=u-${n} svc=${path}`).join("\n") : "body" }))
  let step = 0, id = 0, problems = [], lastTurns
  const provider = {
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream(context) {
      lastTurns = context.turns ?? []
      const bad = unanswered(lastTurns)
      if (bad.length) problems.push(`before step ${step}: ${bad.join(" ")}`)
      const calls = script[step++]
      if (!calls) { yield { type: "text_delta", delta: "done" }; return }
      for (const c of calls) yield { type: "tool_call", id: `c${++id}`, name: c.name, arguments: c.arguments }
    },
  }
  let error
  try {
    // first session seeds a record so searches have hits
    if (seed) await store.put(agentId, { record_id: "rec-seed", scope, name: "seed-rule", kind: "project", content: "seed rules content about counting", description: "rules for counting", provenance: { author: "model", trust: "untrusted", evidence_refs: [] }, created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.5, links: [], pinned: false })
    const runner = new RuntimeRunner({ provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 32000, maxTurns: 40, baselineToolIds: ["read_file"],
      agentId, memoryScope: scope, memoryStore: store, memoryCandidateSink: sink, memoryRuntimePolicy: { retrieval: "host", modelWrites: "candidate_sink" }, contextPolicy: { notesBudgetTokens: 600 } })
    for await (const ev of runner.run({ sessionId: label, goal: "count rules" })) if (ev.type === "error") error ??= ev.message
  } catch (e) { error = e?.message ?? String(e) }
  const flag = problems.length || error ? "FAIL" : "ok  "
  if (problems.length || error) process.exitCode = 1
  console.log(`${flag} ${label}${problems.length ? "\n     " + problems.join("\n     ") : ""}${error ? "\n     error: " + String(error).slice(0, 200) : ""}`)
  if ((problems.length || error) && process.env.DUMP) console.log(JSON.stringify(lastTurns, null, 1).slice(0, 4000))
}

const S = {
  "mem": [[C.mem()]],
  "mem+mem": [[C.mem("a"), C.mem("b")]],
  "mem+read": [[C.mem(), C.read()]],
  "read+mem": [[C.read(), C.mem()]],
  "prop": [[C.prop()]],
  "prop+prop": [[C.prop("a"), C.prop("b")]],
  "prop+mem": [[C.prop(), C.mem()]],
  "mem+prop": [[C.mem(), C.prop()]],
  "prop+read": [[C.prop(), C.read()]],
  "note+prop": [[C.note(), C.prop()]],
  "prop+note": [[C.prop(), C.note()]],
  "note+mem": [[C.note(), C.mem()]],
  "mem+note": [[C.mem(), C.note()]],
  "note+mem+read": [[C.note(), C.mem(), C.read()]],
  "fetch": [[C.fetch("rec-seed")]],
  "fetch-missing": [[C.fetch("nope")]],
  "fetch+mem": [[C.fetch("rec-seed"), C.mem()]],
  "fetch+prop": [[C.fetch("rec-seed"), C.prop()]],
  "mem+fetch+prop+note": [[C.mem(), C.fetch("rec-seed"), C.prop(), C.note()]],
  "prop,then mem": [[C.prop()], [C.mem()], [C.read()]],
  "mem x3 turns": [[C.mem("a")], [C.mem("b")], [C.mem("c"), C.prop()]],
  "rep-mem x14": Array.from({ length: 14 }, () => [C.mem("same")]),
  "rep-mem-pairs x8": Array.from({ length: 8 }, () => [C.mem("same"), C.mem("same")]),
  "rep-read x14": Array.from({ length: 14 }, () => [C.read()]),
  "rep-read+mem x10": Array.from({ length: 10 }, () => [C.read(), C.mem("same")]),
  "rep-mem+read x10": Array.from({ length: 10 }, () => [C.mem("same"), C.read()]),
  "rep-note x10": Array.from({ length: 10 }, () => [C.note()]),
  "rep-prop x8": Array.from({ length: 8 }, () => [C.prop()]),
  "rep-fetch x10": Array.from({ length: 10 }, () => [C.fetch("rec-seed")]),
  "rep-fetch+read x10": Array.from({ length: 10 }, () => [C.fetch("rec-seed"), C.read()]),
  "note-big x6": Array.from({ length: 6 }, (_, i) => [C.note("T" + i), { name: "note", arguments: { title: "Big" + i, body: "word ".repeat(400) } }, C.read()]),
  "unknown-tool+mem": [[{ name: "grep", arguments: {} }, C.mem()]],
  "unknown-tool": [[{ name: "grep", arguments: {} }], [C.read()]],
  "bad-both": [[{ name: "memory", arguments: { query: "x", record_id: "rec-seed" } }]],
  "bad-both+read": [[{ name: "memory", arguments: { query: "x", record_id: "rec-seed" } }, C.read()]],
  "mem-empty-args": [[{ name: "memory", arguments: {} }]],
  "mem-empty+prop": [[{ name: "memory", arguments: {} }, C.prop()]],
  "prop-bad+mem": [[{ name: "propose_memory", arguments: { name: "x" } }, C.mem()]],
  "note-bad+mem": [[{ name: "note", arguments: {} }, C.mem()]],
}
if (process.env.FUZZ) {
  let state = Number(process.env.FUZZ)
  const rnd = () => { state ^= state << 13; state >>>= 0; state ^= state >>> 17; state ^= state << 5; state >>>= 0; return state / 4294967296 }
  for (let run = 0; run < Number(process.env.RUNS ?? 40); run++) {
    let u = 0
    const one = () => { const r = rnd(); u++; return r < 0.45 ? { name: "read_file", arguments: { path: "logs/f" + u } } : r < 0.7 ? C.mem("rules counting " + u) : r < 0.8 ? C.fetch("rec-seed") : r < 0.9 ? C.note("N" + (u % 3)) : C.prop("rule-" + (u % 2)) }
    const script = Array.from({ length: 30 }, () => {
      const calls = Array.from({ length: 1 + Math.floor(rnd() * 3) }, one)
      const hasProp = calls.some(c => c.name === "propose_memory")
      // known-bad combination (reported separately): propose_memory beside memory in one turn
      return hasProp ? calls.filter(c => c.name !== "memory") : calls
    })
    await scenario("fuzz#" + run + " " + script.map(t => t.map(c => c.name[0] + (c.arguments.record_id ? "f" : "")).join("")).join(" "), script)
  }
  process.exit(process.exitCode ?? 0)
}
for (const [label, script] of Object.entries(S).filter(([l]) => !process.env.ONLY || l === process.env.ONLY)) {
  await scenario(label, script)
  await scenario(label + " [empty store]", script, { seed: false })
  await scenario(label + " [slow sink]", script, { slowSink: 30 })
}
