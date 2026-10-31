// Real WASM kernel settlement, journal replay and host routing (no mocked commit).
import assert from "node:assert/strict"
import { register } from "node:module"
import { pathToFileURL } from "node:url"
const kernelUrl = new URL("../pkg-node/deepstrike_wasm.js", import.meta.url).href
register(`data:text/javascript,${encodeURIComponent(`export async function resolve(s,c,n) {
  if (s === "@deepstrike/wasm-kernel") return {url:${JSON.stringify(kernelUrl)},shortCircuit:true}; return n(s,c)
}`)}`, pathToFileURL("./"))
const { CanonicalKernel } = await import("@deepstrike/wasm-kernel")
const { CanonicalRunnerRuntime } = await import("../dist/runtime/canonical-kernel-step.js")
const { InMemoryKernelJournal } = await import("../dist/runtime/kernel-journal.js")
const { RuntimeRunner, collectText } = await import("../dist/runtime/runner.js")
const { InMemorySessionLog } = await import("../dist/runtime/session-log.js")
const { LocalExecutionPlane } = await import("../dist/runtime/execution-plane.js")
const { InMemoryMemoryCandidateSink, memoryCandidateId, memoryCandidatePayloadDigest } = await import("../dist/memory/candidate-sink.js")
const { memoryCandidateAttestationDigest } = await import("../dist/memory/attestation.js")
const off = { retrieval: "disabled", modelWrites: "disabled" }
const provider = { async complete() { throw Error("unused") }, async *stream() { yield {type:"text_delta",delta:"done"} } }
const scope = { tenant_id: "agent", namespace: "candidates" }
let checks = 0
for (const status of ["pending", "accepted", "rejected", "disabled"]) {
  let puts = 0
  const store = { async put() { puts++ }, async search() { return [] }, async saveSession() {} }
  const held = new InMemoryMemoryCandidateSink()
  let drop = true
  const sink = { async submit(agent, candidate) {
    if (status === "pending") {
      const receipt = await held.submit(agent, candidate)
      if (drop) { drop = false; throw Error("lost acknowledgement") }
      return receipt
    }
    return { status, candidate_id: candidate.candidate_id, receipt_id: "host-receipt",
      ...(status === "accepted" ? {record_ref:"host:r",digest:"sha256:"+"0".repeat(64)} : {reason_code:"source_unverifiable"}) }
  } }
  const runner = new RuntimeRunner({ provider, sessionLog:new InMemorySessionLog(), executionPlane:new LocalExecutionPlane(),
    maxTokens:10000, agentId:"agent", memoryScope:scope, memoryStore:store, memoryCandidateSink:sink,
    memoryRuntimePolicy:{...off,modelWrites:status === "disabled" ? "disabled" : "candidate_sink"} })
  const journal = new InMemoryKernelJournal()
  const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, `op-native-${status}`, {maxContextTokens:10000})
  await rt.applyHostEvent({kind:"set_memory_enabled",enabled:true,read:false,write:status !== "disabled"})
  let action = await rt.startWorkflow({nodes:[
    {task:{goal:"remember formatting"},role:"explore"}, {task:{goal:"keep workflow open"},role:"explore"},
  ]})
  assert.equal(action.kind,"spawn_workflow")
  const child = action.nodes[0]
  await rt.applyHostEvent({kind:"workflow_spawn_result",effect_id:action.effectId})
  action = await rt.commit({kind:"deliver_external_event",event:{kind:"child_completed",
    task_id:child.agent_id,attempt_id:child.attempt_id,
    result:{status:"completed",usage:{total_tokens:"0",turns:1}},
    parent_requests:[{kind:"request_memory_write",proposal:{name:"brief-style",kind:"project",content:"numbered sections",description:"format",evidence_refs:[]}}],
  }})
  if (status === "disabled") {
    assert.notEqual(action?.kind,"persist_memory")
    assert(rt.drainHostObservations().some(o => o.kind === "control_request_rejected"))
    checks++; continue
  }
  assert.equal(action.kind,"persist_memory")
  let result = await runner.persistMemoryEffectResult(rt.operationId,action.effectId,action.memory)
  if (status === "pending") {
    assert.match(result.error,/lost acknowledgement/)
    result = await runner.persistMemoryEffectResult(rt.operationId,action.effectId,action.memory)
    assert.equal(held.candidates().length,1)
    assert.equal(result.receipt_id,"receipt:1")
  }
  await rt.applyHostEvent(result)
  const observations = rt.drainHostObservations()
  assert.equal(puts,0)
  assert.equal(observations.some(o => o.kind === "memory_written"),status === "accepted")
  if (status === "pending") {
    assert(observations.some(o => o.kind === "memory_candidate_pending"))
    assert.notEqual(rt.resumeAction()?.kind,"persist_memory")
    const count = (await journal.recordsAfter(rt.operationId)).length
    await rt.applyHostEvent(result)
    assert.equal((await journal.recordsAfter(rt.operationId)).length,count,"identical outcome replays without another commit")
    await assert.rejects(rt.applyHostEvent({...result,candidate_id:"different-candidate"}))
    const restored = new CanonicalKernel()
    restored.restore(undefined,(await journal.recordsAfter(rt.operationId)).map(r => r.record_bytes))
    assert(!restored.projectionJson().includes('"kind":"persist_memory"'))
  }
  checks++
}
// Identity comes from the real kernel and matches the vectors every SDK pins.
assert.equal(await memoryCandidateId(["child_request", "op-1", "effect-3", "0"]),
  "candidate:ead40e56240c408c06eeb95c7fe297bbb3b3c187c8cf181e0227aa269bcd7043")
assert.equal(await memoryCandidateAttestationDigest({
  issuer: "issuer-a", key_id: "ed-key-1", agent_id: "agent", tenant_id: "t", namespace: "n",
  candidate_id: "host-candidate", source: "model_proposal",
  payload_digest: "sha256:dfac225c1c832a2900dd59d0867840abbf7e396ef13d03308b7c5ff7516e145f",
  issued_at: 1000, expires_at: 2000, policy_version: "memory-attestation-v1", signature: "",
}), "sha256:d69f2de333253735f34e09cd5ef2d8d9c2c71b9b066d302e9a7716bb8b19b63e")
assert.match(await memoryCandidatePayloadDigest({
  record_id: "memory:1", scope, name: "n", kind: "user", content: "c", description: "",
  provenance: { author: "model", trust: "untrusted", evidence_refs: [] },
  created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.000001, links: [], pinned: false,
}), /^sha256:[0-9a-f]{64}$/)
checks++
console.log(`Passed ${checks} real WASM candidate scenarios`)
