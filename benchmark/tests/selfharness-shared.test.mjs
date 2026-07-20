/**
 * Self-Harness shared layer (V2-S4) tests — Node built-in runner.
 *
 * Run:  node --test benchmark/tests/selfharness-shared.test.mjs
 *
 * v2 ships the shared layer's TYPES + GATE only. These tests pin the two invariants that make a
 * cross-tenant promotion safe: (1) `aggregateSharedEvidence` strips every scope's verbatim transcript
 * (no excerpt text, no taskIds survive serialization) while keeping signatures + counts; (2)
 * `promoteToShared` throws unless the signature recurs in ≥2 distinct scopes AND an explicit human
 * approval token is present — there is no auto path.
 */

import assert from "node:assert/strict"
import { test } from "node:test"

import { aggregateSharedEvidence, promoteToShared } from "../selfharness/shared.mjs"

const SIG = { cause: "timeout:C_locate", symptom: "clean" }
const KEY = JSON.stringify(SIG)
const SIG2 = { cause: "no_progress:C_recover", symptom: "denied" }

/** A minimal per-scope EvidenceBundle carrying one cluster whose excerpt + taskIds are DISTINCTIVE
 *  markers, so a leak into the aggregate is detectable by substring. */
function bundleFor(scope, signature, size, taskIds, excerptText) {
  return {
    round: 0,
    scope,
    harnessDigest: "d",
    totals: { tasks: size, passed: 0, failed: size },
    clusters: [{
      key: JSON.stringify(signature),
      signature,
      size,
      taskIds,
      toolUsage: { read_file: { calls: size, errors: size } },
      excerpt: [{ taskId: taskIds[0], text: excerptText }],
    }],
    passingNote: { count: 0, medianTurns: null, medianTokens: null },
    previousAttempts: [],
    provenance: "x",
  }
}

const ALICE = bundleFor("alice", SIG, 2, ["alice-task-XYZ"], "SECRET_ALICE_TRANSCRIPT read the wrong file")
const BOB = bundleFor("bob", SIG, 3, ["bob-task-QRS"], "SECRET_BOB_TRANSCRIPT denied twice")

// ── aggregateSharedEvidence ─────────────────────────────────────────────────────

test("aggregates one signature across two scopes into totalSize + per-scope counts", () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  assert.equal(agg.clusters.length, 1)
  const c = agg.clusters[0]
  assert.deepEqual(c.signature, SIG)
  assert.equal(c.totalSize, 5)
  assert.deepEqual(c.scopes, [{ scope: "alice", size: 2 }, { scope: "bob", size: 3 }]) // scope-name sorted
})

test("the serialized aggregate contains NO excerpt text and NO taskIds (cross-tenant privacy)", () => {
  const serialized = JSON.stringify(aggregateSharedEvidence([ALICE, BOB]))
  for (const forbidden of [
    "SECRET_ALICE_TRANSCRIPT", "SECRET_BOB_TRANSCRIPT", // excerpt text
    "alice-task-XYZ", "bob-task-QRS",                   // taskIds
    "read the wrong file", "denied twice",              // excerpt prose
    "read_file",                                         // toolUsage tool names
  ]) {
    assert.ok(!serialized.includes(forbidden), `aggregate leaked "${forbidden}"`)
  }
  // Signatures, scope NAMES, and counts DO survive — that is the whole point.
  assert.ok(serialized.includes("timeout:C_locate"))
  assert.ok(serialized.includes("alice") && serialized.includes("bob"))
})

test("distinct signatures become distinct clusters, name-sorted by signature key", () => {
  const other = bundleFor("carol", SIG2, 1, ["carol-1"], "SECRET_CAROL")
  const agg = aggregateSharedEvidence([BOB, other, ALICE])
  assert.equal(agg.clusters.length, 2)
  const keys = agg.clusters.map(c => JSON.stringify(c.signature))
  assert.deepEqual(keys, [...keys].sort()) // name-sorted, deterministic
})

test("requires at least one bundle", () => {
  assert.throws(() => aggregateSharedEvidence([]), /at least one/)
  assert.throws(() => aggregateSharedEvidence(null), /at least one/)
})

// ── promoteToShared gate ────────────────────────────────────────────────────────

const patchFor = (targetSurface, targetCluster = KEY) => ({
  targetSurface, op: "set", value: targetSurface.startsWith("runtime.") ? ["read"] : "run tests",
  rationale: "the same failure recurs across tenants", targetCluster, expectedEffect: "e",
})

test("happy path — ≥2 scopes + human approval ⇒ a frozen proposal recording tier + scopes", async () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  const patch = patchFor("instructions.verification")
  const proposal = await promoteToShared({ patch, aggregate: agg, decision: { approvedBy: "secops@corp" } })
  assert.equal(proposal.patch, patch)
  assert.equal(proposal.tier, "screened")           // instructions.* is Tier B — recorded, not gated on here
  assert.deepEqual(proposal.scopes, ["alice", "bob"])
  assert.equal(proposal.approvedBy, "secops@corp")
  assert.ok(Object.isFrozen(proposal))
})

test("records a Tier A surface's tier too (any tier may be shared, but it is recorded)", async () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  const proposal = await promoteToShared({ patch: patchFor("runtime.allowedToolIds"), aggregate: agg, decision: { approvedBy: "x" } })
  assert.equal(proposal.tier, "auto")
})

test("throws when the signature appears in only ONE scope", async () => {
  const single = aggregateSharedEvidence([ALICE]) // alice only
  await assert.rejects(
    () => promoteToShared({ patch: patchFor("instructions.verification"), aggregate: single, decision: { approvedBy: "x" } }),
    /≥2 distinct scopes/,
  )
})

test("throws when the human-approval token is missing or empty (no auto path)", async () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  const patch = patchFor("instructions.verification")
  await assert.rejects(() => promoteToShared({ patch, aggregate: agg, decision: {} }), /approvedBy/)
  await assert.rejects(() => promoteToShared({ patch, aggregate: agg, decision: { approvedBy: "" } }), /approvedBy/)
  await assert.rejects(() => promoteToShared({ patch, aggregate: agg }), /approvedBy/)
})

test("throws when the patch's targetCluster is absent from the aggregate", async () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  const patch = patchFor("instructions.verification", JSON.stringify({ cause: "nope:zz", symptom: "clean" }))
  await assert.rejects(
    () => promoteToShared({ patch, aggregate: agg, decision: { approvedBy: "x" } }),
    /absent from the shared aggregate/,
  )
})

test("throws when the patch surface has no tier (unknown surface)", async () => {
  const agg = aggregateSharedEvidence([ALICE, BOB])
  const patch = patchFor("governance.limits")
  await assert.rejects(
    () => promoteToShared({ patch, aggregate: agg, decision: { approvedBy: "x" } }),
    /unknown surface path/,
  )
})
