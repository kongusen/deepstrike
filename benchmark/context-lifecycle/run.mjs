#!/usr/bin/env node
// Context-lifecycle baseline (plan task 4.3, first cut). Deterministic: scripted providers drive the
// real kernel, so two checkouts can be compared byte for byte without an API key.
//
//   node benchmark/context-lifecycle/run.mjs [--repo <checkout>] [--json]
//
// <checkout> defaults to this repository; its `node/dist` must be built. Metrics are character
// counts of what each provider call would send, so they measure the context the kernel renders,
// not model quality.

import { resolve } from "node:path"
import { pathToFileURL } from "node:url"

const args = process.argv.slice(2)
const repo = resolve(args.includes("--repo") ? args[args.indexOf("--repo") + 1] : new URL("../..", import.meta.url).pathname)
const asJson = args.includes("--json")

const dist = file => import(pathToFileURL(resolve(repo, "node/dist", file)).href)
const root = await dist("index.js")
const advanced = await dist("advanced/public.js")
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = advanced
const { tool } = root

// ── measurement ────────────────────────────────────────────────────────────────────────────────

/** The cacheable request prefix, in Anthropic order: system[0], system[1], history turns. */
function cacheable(context) {
  return JSON.stringify([context.systemStable ?? context.systemText ?? "", context.systemKnowledge ?? "", context.turns ?? []])
}

function commonPrefix(a, b) {
  const n = Math.min(a.length, b.length)
  let i = 0
  while (i < n && a.charCodeAt(i) === b.charCodeAt(i)) i++
  return i
}

/** Per-session call log: input size, how much of it repeats the previous call's prefix, and how
 *  often system[1] (knowledge) changed under a cached history. */
class CallMeter {
  constructor() { this.sessions = new Map() }
  record(key, context) {
    const prefix = cacheable(context)
    const input = prefix.length + JSON.stringify(context.stateTurn ?? "").length
    const session = this.sessions.get(key) ?? { calls: [], lastPrefix: "", lastKnowledge: undefined }
    const reused = commonPrefix(session.lastPrefix, prefix)
    const knowledge = context.systemKnowledge ?? ""
    session.calls.push({
      input,
      reused,
      knowledgeChanged: session.lastKnowledge !== undefined && knowledge !== session.lastKnowledge,
      historyTurns: (context.turns ?? []).length,
    })
    session.lastPrefix = prefix
    session.lastKnowledge = knowledge
    this.sessions.set(key, session)
    return session.calls.length
  }
  summary(key) {
    const calls = this.sessions.get(key)?.calls ?? []
    const input = calls.reduce((sum, call) => sum + call.input, 0)
    const reused = calls.reduce((sum, call) => sum + call.reused, 0)
    return {
      calls: calls.length,
      inputChars: input,
      firstCallInputChars: calls[0]?.input ?? 0,
      prefixReuse: input ? Number((reused / input).toFixed(3)) : 0,
      knowledgeChangesUnderCache: calls.filter(call => call.knowledgeChanged && call.historyTurns > 1).length,
    }
  }
}

// ── fixtures ───────────────────────────────────────────────────────────────────────────────────

const page = n => `page ${n}: ` + Array.from({ length: 60 }, (_, i) => `fact-${n}-${i} lorem ipsum dolor sit amet`).join("; ")
const scope = { tenant_id: "bench", namespace: "context-lifecycle" }

function memoryStore() {
  const records = [1, 2, 3].map(i => ({
    record_id: `rec-${i}`, scope, name: `note-${i}`, kind: "project",
    content: `long body of note ${i} `.repeat(40), description: `how area ${i} is laid out`,
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
  }))
  return {
    put: async () => {}, delete: async () => {}, saveSession: async () => {},
    get: async (_agent, id) => records.find(record => record.record_id === id) ?? null,
    search: async (_agent, query) => records.slice(0, query.top_k ?? 3).map(record => ({ record, score: 0.9, why: "bench" })),
  }
}

async function drain(stream) {
  for await (const _event of stream) { /* drain */ }
}

async function lifecycleReport(sessionLog, sessionId) {
  const events = await sessionLog.read(sessionId)
  const report = events.map(entry => entry.event).find(event => event.kind === "context_lifecycle_reported")
  if (!report) return null
  const { kind: _kind, turn: _turn, ...metrics } = report
  return metrics
}

// ── scenario A: one long tool-heavy run with memory and mid-run knowledge ──────────────────────

async function longRun() {
  const meter = new CallMeter()
  const sessionLog = new InMemorySessionLog()
  const plane = new LocalExecutionPlane()
  let runner
  plane.register(tool("fetch_page", "fetch one page", { type: "object", properties: { n: { type: "integer" } } }, async ({ n }) => {
    // Mid-run knowledge arrives twice, as a host integration would push a fresh reference.
    if (n === 6 || n === 14) await runner.pushKnowledge({ role: "system", content: `reference sheet ${n}: ` + "x".repeat(800), toolCalls: [] }, undefined, { key: `ref-${n}` })
    return page(n)
  }))
  const provider = {
    async complete() { return { role: "assistant", content: "unused", toolCalls: [] } },
    async *stream(context) {
      const call = meter.record("root", context)
      if (call <= 24) yield { type: "tool_call", id: `c${call}`, name: "fetch_page", arguments: { n: call } }
      else yield { type: "text_delta", delta: "summary done" }
    },
  }
  runner = new RuntimeRunner({
    provider, sessionLog, executionPlane: plane,
    maxTokens: 12_000, maxTurns: 40, baselineToolIds: ["fetch_page"],
    agentId: "bench-agent", memoryScope: scope, memoryStore: memoryStore(),
  })
  await drain(runner.run({ sessionId: "long-run", goal: "survey every page and summarize the areas" }))
  return { ...meter.summary("root"), lifecycle: await lifecycleReport(sessionLog, "long-run") }
}

// ── scenario B: fan-out(3) → synthesize, with large dependency outputs ─────────────────────────

async function fanOut(dependencyMode) {
  const meter = new CallMeter()
  const sessionLog = new InMemorySessionLog()
  let synthesizeRead = false
  const provider = {
    async complete() { return { role: "assistant", content: "unused", toolCalls: [] } },
    async *stream(context, tools) {
      const text = JSON.stringify([context.turns, context.stateTurn])
      const area = /survey area-(\d)/.exec(text)?.[1]
      if (area && !text.includes("synthesize")) {
        meter.record(`area-${area}`, context)
        yield { type: "text_delta", delta: `findings for area ${area}: ` + `detail-${area} `.repeat(1500) }
        return
      }
      const call = meter.record("synthesize", context)
      const offered = (tools ?? []).map(t => t.name)
      if (call === 1 && offered.includes("read_result")) {
        yield { type: "tool_call", id: "read-0", name: "read_result", arguments: { call_id: "dep:wf-node0" } }
        return
      }
      if (text.includes("[PAYLOAD handle_id=dep:wf-node0]")) synthesizeRead = true
      yield { type: "text_delta", delta: "synthesis done" }
    },
  }
  const runner = new RuntimeRunner({ provider, sessionLog, executionPlane: new LocalExecutionPlane(), maxTokens: 64_000, maxTurns: 6 })
  const context = dependencyMode ? { dependencyMode } : undefined
  const outcome = await runner.runWorkflow({
    nodes: [
      { task: "survey area-1", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
      { task: "survey area-2", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
      { task: "survey area-3", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
      { task: "synthesize the three surveys", role: "plan", dependsOn: [0, 1, 2], ...(context ? { context } : {}) },
    ],
  }, { sessionId: `fan-out-${dependencyMode ?? "default"}` })
  const synth = meter.summary("synthesize")
  return {
    completed: outcome.nodeOutcomes.filter(node => node.status === "completed").length,
    synthesizeFirstCallInputChars: synth.firstCallInputChars,
    synthesizeCalls: synth.calls,
    synthesizeReadFullDependency: synthesizeRead,
  }
}

// ── report ─────────────────────────────────────────────────────────────────────────────────────

const report = {
  repo,
  longRun: await longRun(),
  fanOutDefault: await fanOut(undefined),
  fanOutHandle: await fanOut("handle"),
}
if (asJson) console.log(JSON.stringify(report, null, 2))
else {
  console.log(`checkout: ${repo}`)
  for (const [name, metrics] of Object.entries(report)) {
    if (name === "repo") continue
    console.log(`\n${name}`)
    for (const [key, value] of Object.entries(metrics)) console.log(`  ${key}: ${typeof value === "object" ? JSON.stringify(value) : value}`)
  }
}
