/**
 * Self-Harness miner / proposer / merge unit tests — Node built-in runner.
 *
 * Run:  node --test benchmark/tests/selfharness-propose.test.mjs
 *
 * Covers the LLM-slot plumbing in isolation: the miner's JSON-parse retry-then-discard policy and the
 * addressability filter; the proposer's tolerant JSON extraction, illegal-patch discard, and K cap; and
 * `mergeAccepted`'s disjoint-merge vs surface-conflict resolution. The model is a canned `complete()`.
 */

import assert from "node:assert/strict"
import { test } from "node:test"

import { mineMechanisms } from "../selfharness/miner.mjs"
import { propose } from "../selfharness/proposer.mjs"
import { mergeAccepted } from "../selfharness/validate.mjs"
import { fixtureSeedManifest } from "../selfharness/adapters/fixture.mjs"
import { loadSdk } from "../utils/sdk.mjs"

const { applyPatch, manifestDigest } = await loadSdk()

function cannedComplete(responses) {
  let i = 0
  return async () => {
    const r = responses[i++]
    if (r === undefined) throw new Error(`cannedComplete: no response for call #${i}`)
    return r
  }
}

const oneClusterBundle = (key = "k1") => ({
  round: 0,
  harnessDigest: "d",
  totals: { tasks: 1, passed: 0, failed: 1 },
  clusters: [{ key, signature: { cause: "timeout:C1", symptom: "clean" }, size: 1, taskIds: ["t1"], excerpt: [] }],
  passingNote: { count: 0, medianTurns: null, medianTokens: null },
  previousAttempts: [],
})

// ── miner ─────────────────────────────────────────────────────────────────────

test("miner parses a valid attribution and keeps addressable clusters", async () => {
  const complete = cannedComplete([
    JSON.stringify({ mechanism: "missing recovery", addressable: true, reasoning: "no retry guidance" }),
  ])
  const attrs = await mineMechanisms({ bundle: oneClusterBundle(), complete })
  assert.equal(attrs.length, 1)
  assert.equal(attrs[0].clusterKey, "k1")
  assert.equal(attrs[0].mechanism, "missing recovery")
  assert.equal(attrs[0].addressable, true)
})

test("miner extracts JSON wrapped in prose / code fences", async () => {
  const complete = cannedComplete([
    'Here is my analysis:\n```json\n{"mechanism":"x","addressable":false,"reasoning":"y"}\n```\nThanks.',
  ])
  const attrs = await mineMechanisms({ bundle: oneClusterBundle(), complete })
  assert.equal(attrs.length, 1)
  assert.equal(attrs[0].addressable, false)
})

test("miner retries once on a parse failure, then succeeds", async () => {
  const complete = cannedComplete([
    "sorry, no json here",
    JSON.stringify({ mechanism: "recovered", addressable: true, reasoning: "ok" }),
  ])
  const attrs = await mineMechanisms({ bundle: oneClusterBundle(), complete })
  assert.equal(attrs.length, 1)
  assert.equal(attrs[0].mechanism, "recovered")
})

test("miner discards a cluster's attribution after two parse failures", async () => {
  const complete = cannedComplete(["garbage one", "garbage two"])
  const attrs = await mineMechanisms({ bundle: oneClusterBundle(), complete })
  assert.equal(attrs.length, 0)
})

test("miner mines at most the top 4 clusters", async () => {
  const clusters = Array.from({ length: 6 }, (_, i) => ({
    key: `k${i}`, signature: { cause: `c${i}`, symptom: "clean" }, size: 6 - i, taskIds: [`t${i}`], excerpt: [],
  }))
  const bundle = { ...oneClusterBundle(), clusters }
  let calls = 0
  const complete = async () => {
    calls++
    return JSON.stringify({ mechanism: "m", addressable: true, reasoning: "r" })
  }
  const attrs = await mineMechanisms({ bundle, complete })
  assert.equal(calls, 4)
  assert.equal(attrs.length, 4)
})

// ── proposer ────────────────────────────────────────────────────────────────

const clusterFor = manifest => ({
  ...oneClusterBundle(),
  clusters: [{ key: "k1", signature: { cause: "completed:VK_verify", symptom: "clean" }, size: 1, taskIds: ["verify-keyword"], excerpt: [], mechanism: "missing verification" }],
})

test("proposer discards an illegal patch and keeps the legal one", async () => {
  const manifest = fixtureSeedManifest()
  const complete = cannedComplete([JSON.stringify([
    { targetSurface: "instructions.verification", op: "set", value: "run tests", rationale: "r", targetCluster: "k1", expectedEffect: "e" },
    { targetSurface: "governance.limits", op: "set", value: "x", rationale: "r", targetCluster: "k1", expectedEffect: "e" },
  ])])
  const { patches, discarded } = await propose({ bundle: clusterFor(manifest), manifest, k: 4, complete })
  assert.equal(patches.length, 1)
  assert.equal(patches[0].targetSurface, "instructions.verification")
  assert.equal(discarded.length, 1)
  assert.match(discarded[0].reason, /whitelist/)
})

test("proposer enforces the K cap (slices before validating)", async () => {
  const manifest = fixtureSeedManifest()
  const three = [1, 2, 3].map(n => ({
    targetSurface: "instructions.verification", op: "set", value: `run tests ${n}`, rationale: "r", targetCluster: "k1", expectedEffect: "e",
  }))
  const { patches } = await propose({ bundle: clusterFor(manifest), manifest, k: 2, complete: cannedComplete([JSON.stringify(three)]) })
  assert.equal(patches.length, 2)
})

test("proposer returns empty patches on non-array output", async () => {
  const manifest = fixtureSeedManifest()
  const { patches, discarded } = await propose({ bundle: clusterFor(manifest), manifest, k: 2, complete: cannedComplete(['{"not":"an array"}']) })
  assert.equal(patches.length, 0)
  assert.equal(discarded.length, 1)
  assert.match(discarded[0].reason, /not a JSON array/)
})

test("proposer prompt shows editable surfaces with current values", async () => {
  const manifest = fixtureSeedManifest()
  const { prompt } = await propose({ bundle: clusterFor(manifest), manifest, k: 1, complete: cannedComplete(["[]"]) })
  assert.match(prompt, /instructions\.execution = "Work step by step and cite sources/)
  assert.match(prompt, /nudges = \(unset\)/)
})

// ── mergeAccepted ─────────────────────────────────────────────────────────────

const patch = (surface, value, op = "set") => ({
  targetSurface: surface, op, value, rationale: "r", targetCluster: "k1", expectedEffect: "e",
})

test("mergeAccepted merges disjoint surfaces into one manifest", async () => {
  const manifest = fixtureSeedManifest()
  const accepted = [
    { patch: patch("instructions.verification", "run tests"), dIn: 1, dHo: 0 },
    { patch: patch("nudges", { id: "n1", on: { kind: "tool_error" }, note: "recover after an error" }, "append"), dIn: 0, dHo: 1 },
  ]
  const { manifest: merged, merged: kept, skipped } = await mergeAccepted(manifest, accepted)
  assert.equal(kept.length, 2)
  assert.equal(skipped.length, 0)
  assert.equal(merged.instructions.verification, "run tests")
  assert.equal(merged.nudges.length, 1)
  assert.equal(merged.nudges[0].id, "n1")
  // lineage normalized to the round-start manifest.
  assert.equal(merged.parent, manifestDigest(manifest))
})

test("mergeAccepted resolves a same-surface conflict, keeping the higher combined delta", async () => {
  const manifest = fixtureSeedManifest()
  const strong = { patch: patch("instructions.verification", "STRONG run tests"), dIn: 1, dHo: 1, decision: { accepted: true } }
  const weak = { patch: patch("instructions.verification", "weak run tests"), dIn: 1, dHo: 0, decision: { accepted: true } }
  const { manifest: merged, merged: kept, skipped } = await mergeAccepted(manifest, [weak, strong])
  assert.equal(kept.length, 1)
  assert.equal(kept[0].patch.value, "STRONG run tests")
  assert.equal(merged.instructions.verification, "STRONG run tests")
  assert.equal(skipped.length, 1)
  assert.equal(skipped[0].reason, "skipped_conflict")
  assert.equal(skipped[0].patch.value, "weak run tests")
})

test("mergeAccepted with no accepted edits returns the manifest unchanged", async () => {
  const manifest = fixtureSeedManifest()
  const { manifest: merged, merged: kept } = await mergeAccepted(manifest, [])
  assert.equal(kept.length, 0)
  assert.equal(manifestDigest(merged), manifestDigest(manifest))
})

test("evaluate survives a throwing adapter: the repeat counts as a failed run", async () => {
  const { evaluate } = await import("../selfharness/validate.mjs")
  const tasks = [
    { id: "ok", goal: "g", criteria: [{ text: "c" }] },
    { id: "boom", goal: "g", criteria: [{ text: "c" }] },
  ]
  const adapter = {
    id: "stub",
    listTasks: () => tasks,
    async runTask(task) {
      if (task.id === "boom") throw new Error("kernel refused ConfigureRun")
      return { passed: true, verdict: { passed: true, overallScore: 1, feedback: "", details: [] }, events: [], termination: "completed" }
    },
  }
  const { passCount, results } = await evaluate({ manifest: fixtureSeedManifest(), adapter, tasks, repeats: 1 })
  assert.equal(passCount, 1)
  const boom = results.find(r => r.taskId === "boom")
  assert.equal(boom.passed, false)
  assert.equal(boom.termination, "adapter_error")
  assert.match(boom.verdict.feedback, /kernel refused/)
})
