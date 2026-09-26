// WASM SDK × real kernel (pkg-node) regressions for the 0.2.74 boundary P1 audit.
// Same harness as boundary-p0-regressions.node.mjs: the loader hook maps `@deepstrike/wasm-kernel`
// onto the node-target build so the runner exercises the real canonical kernel.
import assert from "node:assert/strict"
import { register } from "node:module"
import { pathToFileURL } from "node:url"

const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(
  `data:text/javascript,${encodeURIComponent(`
    export async function resolve(specifier, context, next) {
      if (specifier === "@deepstrike/wasm-kernel") return { url: ${JSON.stringify(kernelUrl)}, shortCircuit: true }
      return next(specifier, context)
    }`)}`,
  pathToFileURL("./"),
)

const dist = new URL("../dist/", import.meta.url)
const { CanonicalKernel } = await import("@deepstrike/wasm-kernel")
const { InMemoryKernelJournal } = await import(new URL("runtime/kernel-journal.js", dist).href)
const { CanonicalRunnerRuntime } = await import(new URL("runtime/canonical-kernel-step.js", dist).href)
const { RuntimeRunner, signalToKernelEvent } = await import(new URL("runtime/runner.js", dist).href)
const { InMemorySessionLog } = await import(new URL("runtime/session-log.js", dist).href)
const { LocalExecutionPlane } = await import(new URL("runtime/execution-plane.js", dist).href)
const { tool } = await import(new URL("tools/index.js", dist).href)
const { governancePolicyPatch } = await import(new URL("governance.js", dist).href)

let passed = 0
const ok = name => { passed += 1; console.log(`ok ${passed} - ${name}`) }

/** A provider that plays one scripted turn per call, then answers "done". */
function scripted(turns) {
  const contexts = []
  const toolNames = []
  return {
    contexts,
    toolNames,
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream(context, tools) {
      contexts.push(context)
      toolNames.push((tools ?? []).map(t => t.name))
      for (const event of turns[contexts.length - 1] ?? [{ type: "text_delta", delta: "done" }]) yield event
    },
  }
}

async function drain(runner, sessionId, goal = "go") {
  const events = []
  for await (const event of runner.run({ sessionId, goal })) events.push(event)
  return events
}

// P1-13 · a truncated argument string fails the call; the tool never runs with `{}`.
{
  const executed = []
  const plane = new LocalExecutionPlane()
  plane.register(tool("write", "write", { type: "object", properties: { path: { type: "string" } } }, async args => {
    executed.push(args)
    return "wrote"
  }))
  const provider = scripted([[{ type: "tool_call", id: "c1", name: "write", arguments: {}, rawArguments: "{\"path\": \"/tm" }]])
  const runner = new RuntimeRunner({ provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 4, baselineToolIds: ["write"] })
  const events = await drain(runner, "p1-13")
  assert.deepEqual(executed, [])
  const result = events.find(event => event.type === "tool_result")
  assert.equal(result?.isError, true)
  assert.match(result.content, /invalid arguments/)
  ok("P1-13 malformed tool arguments fail the call instead of running it with {}")
}

// A failed tool result reaches the next provider request marked as a failure.
{
  const plane = new LocalExecutionPlane()
  plane.register(tool("fetch", "fetch", { type: "object", properties: {} }, async () => { throw new Error("upstream timeout") }))
  const provider = scripted([[{ type: "tool_call", id: "c-fail", name: "fetch", arguments: {} }]])
  const runner = new RuntimeRunner({ provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 4, baselineToolIds: ["fetch"] })
  await drain(runner, "is-error")
  const parts = provider.contexts[1].turns.flatMap(turn => turn.contentParts ?? [])
  const result = parts.find(part => part.type === "tool_result" && part.callId === "c-fail")
  assert.equal(result?.isError, true)
  ok("a failed tool result reaches the provider marked as a failure")
}

// P1-7 · the model's update_plan progress survives a tool batch.
{
  const plane = new LocalExecutionPlane()
  plane.register(tool("noop", "noop", { type: "object", properties: {} }, async () => "ok"))
  const provider = scripted([
    [{ type: "tool_call", id: "plan", name: "update_plan", arguments: { progress: "drafting section two" } }],
    [{ type: "tool_call", id: "noop", name: "noop", arguments: {} }],
  ])
  const runner = new RuntimeRunner({ provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 5, baselineToolIds: ["noop"], enablePlanTool: true })
  const events = await drain(runner, "p1-7")
  assert.deepEqual(events.filter(event => event.type === "error"), [])
  const third = JSON.stringify(provider.contexts[2])
  assert.ok(third.includes("drafting section two"))
  assert.ok(!third.includes("Executed tools"))
  ok("P1-7 the host never writes the model's task state")
}

// P1-12 · a cancellation during a provider turn names no effect id as a call.
{
  let runner
  const provider = {
    async complete() { return { role: "assistant", content: "", toolCalls: [] } },
    async *stream() { for (let i = 0; i < 50; i += 1) yield { type: "text_delta", delta: "x" } },
  }
  const sessionLog = new InMemorySessionLog()
  runner = new RuntimeRunner({ provider, sessionLog, executionPlane: new LocalExecutionPlane(), maxTokens: 4000, maxTurns: 3 })
  for await (const event of runner.run({ sessionId: "p1-12", goal: "cancel" })) {
    if (event.type === "text_delta") runner.interrupt("user")
  }
  const cancelled = (await sessionLog.read("p1-12")).map(e => e.event).find(e => e.kind === "operation_cancelled")
  assert.deepEqual(cancelled?.pending_call_ids ?? [], [])
  ok("P1-12 pending_call_ids carries logical calls only")
}

// P1-9 · the kernel withholds vetoed tools and renders the note itself.
{
  const plane = new LocalExecutionPlane()
  plane.register(tool("rm", "rm", { type: "object", properties: {} }, async () => "gone"))
  plane.register(tool("ls", "ls", { type: "object", properties: {} }, async () => "files"))
  const provider = scripted([])
  const runner = new RuntimeRunner({
    provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 2,
    baselineToolIds: ["rm", "ls"], governancePolicy: { vetoes: ["rm"], constraints: [{ kind: "range", tool: "ls", path: "n", max: 3 }] },
  })
  const events = await drain(runner, "p1-9")
  assert.deepEqual(events.filter(event => event.type === "error"), [], "governance constraints are accepted by the kernel")
  assert.ok(provider.toolNames[0].includes("ls"))
  assert.ok(!provider.toolNames[0].includes("rm"))
  assert.equal(provider.contexts[0].systemKnowledge.match(/\[governance\]/g)?.length, 1)
  ok("P1-9 the provider sees exactly the kernel-committed surface")
}

// P1-8 · skill content reaches the model through the kernel's admission; a refusal withdraws it.
{
  for (const [name, admitted] of [["debug", true], ["ghost", false]]) {
    // The kernel's catalog is the map's keys at run start. `ghost` is added to the host map only
    // after that, so the host can stage its content but the kernel has never declared it.
    const contentMap = new Map([["debug", "---\nname: debug\n---\nDEBUG-GUIDANCE"]])
    const contexts = []
    const provider = {
      async complete() { return { role: "assistant", content: "", toolCalls: [] } },
      async *stream(context) {
        contexts.push(context)
        if (contexts.length === 1) {
          contentMap.set("ghost", "GHOST-GUIDANCE")
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }
    const runner = new RuntimeRunner({
      provider, sessionLog: new InMemorySessionLog(), executionPlane: new LocalExecutionPlane(),
      maxTokens: 8000, maxTurns: 3, skillContentMap: contentMap,
    })
    const events = await drain(runner, `p1-8-${name}`)
    assert.deepEqual(events.filter(event => event.type === "error"), [])
    const second = JSON.stringify(contexts[1] ?? {})
    if (admitted) assert.ok(second.includes("DEBUG-GUIDANCE"), "an admitted skill's content is rendered")
    else assert.ok(!second.includes("GHOST-GUIDANCE"), "a refused skill's staged content is withdrawn")
  }
  ok("P1-8 skill content follows the kernel's admission")
}

// P1-10 / P1-11 / P1-14 on the canonical runtime directly.
{
  const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-spawn", { maxContextTokens: 100_000 })
  await rt.applyHostEvent({ kind: "configure_run", config: { resource_quota: { max_concurrent_subagents: 4 } } })
  const spawn = await rt.startWorkflow({ nodes: [{ nodeId: "a", task: "a", role: "implement" }, { nodeId: "b", task: "b", role: "implement" }] })
  assert.equal(spawn.kind, "spawn_workflow")
  rt.drainHostObservations()
  await rt.applyHostEvent({
    kind: "workflow_spawn_result", effect_id: spawn.effectId,
    started_agent_ids: ["wf-node0"], failures: [{ agent_id: "wf-node1", error: "no slot", kind: "resource_exhausted" }],
  })
  const batch = rt.drainHostObservations().find(obs => obs.kind === "workflow_batch_spawned")
  assert.deepEqual(batch.nodes.map(node => node.agent_id), ["wf-node0"])
  await assert.rejects(rt.applyHostEvent({ kind: "workflow_spawn_result", effect_id: "nope" }), /not a pending spawn_tasks effect/)
  ok("P1-10 launch outcomes are the host's report, resolved by effect id")
}
{
  const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-signal", { maxContextTokens: 100_000 })
  await rt.startAgent({ goal: "wait" })
  await rt.applyHostEvent(signalToKernelEvent({
    signalId: "s1", deliveryId: "d1", deliveryAttempt: 1,
    signal: { source: "cron", signalType: "job", urgency: "low", payload: { job: "nightly" }, deadlineMs: 2_000 },
  }, 1_000))
  await rt.applyHostEvent(signalToKernelEvent({
    signalId: "s2", deliveryId: "d2", deliveryAttempt: 1,
    signal: { source: "custom", signalType: "event", urgency: "normal", payload: {} }, note: "deploy finished",
  }))
  assert.equal(rt.drainHostObservations().filter(obs => obs.kind === "signal_delivery_disposed").length, 2)
  ok("P1-11 canonical signals (deadline, host note) are admitted by the kernel")
}
{
  const patches = []
  const plane = new LocalExecutionPlane()
  let runner
  plane.register(tool("poke", "poke", { type: "object", properties: {} }, async () => {
    if (patches.length === 0) {
      patches.push(runner.applyPolicyPatch(governancePolicyPatch({ vetoes: ["poke"] })))
      patches.push(runner.applyPolicyPatch(governancePolicyPatch({ vetoes: ["x"] }), { expectedRevision: 9 }))
    }
    return "poked"
  }))
  // The patch lands between effects, so it shows from the render after the one already in hand.
  const provider = scripted([0, 1, 2].map(i => [{ type: "tool_call", id: `p${i}`, name: "poke", arguments: { i } }]))
  runner = new RuntimeRunner({ provider, sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 6, baselineToolIds: ["poke"], repeatFuse: false })
  await drain(runner, "p1-14")
  await patches[0]
  await assert.rejects(patches[1], /revision mismatch/)
  assert.ok(provider.contexts.at(-1).systemKnowledge.includes("[governance]"))
  await assert.rejects(runner.forceCompact(), /requires an active run/)
  ok("P1-14 live policy patches reach the kernel under revision control")
}

// P1-15 · memory has one authority: the kernel validates, meters and counts.
{
  const scope = { tenant_id: "p1-15", namespace: "memory" }
  const stored = (id, recallCount, pinned = false) => ({
    record_id: id, scope, name: id, kind: "reference", content: `${id} body`, description: "fixture",
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: recallCount, confidence: 1, links: [], pinned,
  })
  const store = (hits = []) => {
    const puts = []
    const recalls = []
    return {
      puts, recalls,
      memoryStore: {
        put: async (_agent, record) => { puts.push(record.record_id) },
        get: async () => null, delete: async () => {}, saveSession: async () => {},
        search: async () => hits.map(record => ({ record, score: 0.9, why: "fixture" })),
        recordRecall: async (_agent, batch) => { recalls.push(batch) },
      },
    }
  }

  // a model memory query: the kernel derives the count from the stored one
  {
    const fixture = store([stored("crossing", 2)])
    const promotions = []
    const provider = scripted([[{ type: "tool_call", id: "m1", name: "memory", arguments: { query: "prefs" } }]])
    const runner = new RuntimeRunner({
      provider, sessionLog: new InMemorySessionLog(), executionPlane: new LocalExecutionPlane(), maxTokens: 8000, maxTurns: 4,
      agentId: "p1-15", memoryScope: scope, memoryStore: fixture.memoryStore, preQueryMemory: () => [],
      memoryPolicy: { promotionRecallThreshold: 3 }, onPromotionSuggested: promotion => promotions.push(promotion),
    })
    await drain(runner, "p1-15-query")
    assert.deepEqual(fixture.recalls.flat().map(r => [r.record_id, r.recall_count]), [["crossing", 3]])
    assert.deepEqual(promotions, [{ recordId: "crossing", recallCount: 3 }])
  }

  // a host write inside a live run answers to the kernel's rolling write quota
  {
    const fixture = store()
    const verdicts = []
    let runner
    const plane = new LocalExecutionPlane()
    plane.register(tool("remember_twice", "writes two", { type: "object", properties: {} }, async () => {
      for (const id of ["first", "second"]) verdicts.push(await runner.writeMemory(stored(id, 0)))
      return "ok"
    }))
    runner = new RuntimeRunner({
      provider: scripted([[{ type: "tool_call", id: "w1", name: "remember_twice", arguments: {} }]]),
      sessionLog: new InMemorySessionLog(), executionPlane: plane, maxTokens: 8000, maxTurns: 4, baselineToolIds: ["remember_twice"],
      agentId: "p1-15", memoryScope: scope, memoryStore: fixture.memoryStore, preQueryMemory: () => [],
      resourceQuota: { memoryWritesPerWindow: { maxWrites: 1, windowMs: 60_000 } },
    })
    await drain(runner, "p1-15-quota")
    assert.deepEqual(verdicts, [true, false])
    assert.deepEqual(fixture.puts, ["first"])
  }

  // a host write with no live run is judged by the same kernel rule
  {
    const make = memoryPolicy => new RuntimeRunner({
      provider: scripted([]), sessionLog: new InMemorySessionLog(), agentId: "p1-15",
      memoryStore: store().memoryStore, ...(memoryPolicy ? { memoryPolicy } : {}),
    })
    const wide = { ...stored("wide", 0), name: "记".repeat(100) }
    assert.equal(await make().writeMemory(wide), true, "the name limit counts characters, not bytes")
    assert.equal(await make().writeMemory({ ...wide, name: "记".repeat(101) }), false)
    assert.equal(await make({ maxContentBytes: 4 }).writeMemory({ ...stored("big", 0), content: "12345" }), false)
  }
  ok("P1-15 the kernel is the one memory authority (validation, quota, recall counts)")
}

console.log(`# ${passed} P1 boundary checks passed against the real kernel`)
