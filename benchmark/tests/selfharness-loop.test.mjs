/**
 * Self-Harness loop (S3) e2e test — Node built-in runner, fixture adapter, canned `complete()`.
 *
 * Run:  node --test benchmark/tests/selfharness-loop.test.mjs
 *
 * Drives one full propose→validate→promote round with a deterministic, zero-cost fixture adapter and a
 * canned model (`complete()` returns preset JSON by call order: mine cluster A, mine cluster B, propose).
 * Asserts the paper's contract end-to-end:
 *   ① a good patch (adds the "run tests" verification keyword) is promoted and takes effect
 *   ② a regression patch (removes the execution content a held-out task depends on) is rejected
 *   ③ the lineage digest chain is continuous (child.parent === manifestDigest(parent))
 *   ④ rounds.jsonl carries exactly one line per round, with decisions
 *   ⑤ NO held-out task id or content leaks into any prompt the model saw
 *   ⑥ the non-addressable cluster never enters the proposer's target set
 */

import assert from "node:assert/strict"
import { test } from "node:test"
import { mkdtempSync, readFileSync, rmSync, readdirSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

import { createFixtureAdapter, fixtureSeedManifest, FIXTURE_SPLITS } from "../selfharness/adapters/fixture.mjs"
import { selfHarnessLoop } from "../selfharness/loop.mjs"
import { loadSdk } from "../utils/sdk.mjs"

const { manifestDigest } = await loadSdk()

/** A canned model: returns preset replies by call order, capturing every prompt it was handed. */
function cannedComplete(responses) {
  const prompts = []
  let i = 0
  const fn = async prompt => {
    prompts.push(prompt)
    const r = responses[i++]
    if (r === undefined) throw new Error(`cannedComplete: no response for call #${i}`)
    return r
  }
  fn.prompts = prompts
  return fn
}

// Canned replies for a single round: mine(cluster VK) → addressable, mine(cluster CEILING) →
// NOT addressable, then propose two patches (good + regression).
const MINE_VERIFY = JSON.stringify({
  mechanism: "missing verification protocol",
  addressable: true,
  reasoning: "the model finishes without running tests",
})
const MINE_CEILING = JSON.stringify({
  mechanism: "model capability ceiling",
  addressable: false,
  reasoning: "the optimization is infeasible for this fixed model",
})
const PROPOSE = JSON.stringify([
  {
    targetSurface: "instructions.verification",
    op: "set",
    value: "Before you finish, run tests to verify the change is correct.",
    rationale: "add an explicit verification step so the model checks its work",
    targetCluster: "cluster-verify",
    expectedEffect: "verify-keyword now passes without regressing others",
  },
  {
    targetSurface: "instructions.execution",
    op: "remove",
    rationale: "trim the execution slot to shorten the prompt",
    targetCluster: "cluster-verify",
    expectedEffect: "shorter prompt",
  },
])

async function runRound(lineageDir) {
  const complete = cannedComplete([MINE_VERIFY, MINE_CEILING, PROPOSE])
  const seed = fixtureSeedManifest()
  const result = await selfHarnessLoop({
    seedManifest: seed,
    adapter: createFixtureAdapter(),
    heldIn: FIXTURE_SPLITS.heldIn,
    heldOut: FIXTURE_SPLITS.heldOut,
    rounds: 1,
    k: 2,
    repeats: 1,
    complete,
    lineageDir,
  })
  return { ...result, complete, seed }
}

function withLineageDir(fn) {
  const dir = mkdtempSync(path.join(tmpdir(), "harness-lab-"))
  return fn(dir).finally(() => rmSync(dir, { recursive: true, force: true }))
}

// The fixture seed has no scope and modelProfile "fixture-model" ⇒ lane <dir>/default/fixture-model.
const laneOf = (dir, scope = "default", profile = "fixture-model") => path.join(dir, scope, profile)

test("① good patch is promoted and the final manifest takes effect", async () => {
  await withLineageDir(async dir => {
    const { finalManifest } = await runRound(dir)
    assert.match(finalManifest.instructions.verification, /run tests/)
    // execution slot preserved (the regression edit was rejected).
    assert.match(finalManifest.instructions.execution, /cite sources/)
    // effectiveness: the task that was failing now passes under the promoted harness.
    const adapter = createFixtureAdapter()
    const [task] = adapter.listTasks().filter(t => t.id === "verify-keyword")
    const outcome = await adapter.runTask(task, finalManifest)
    assert.equal(outcome.passed, true)
  })
})

test("② regression patch (removes held-out dependency) is rejected", async () => {
  await withLineageDir(async dir => {
    const { trajectory } = await runRound(dir)
    const round = trajectory[0]
    const exec = round.decisions.find(d => d.surface === "instructions.execution")
    assert.ok(exec, "expected a decision for instructions.execution")
    assert.equal(exec.accepted, false)
    assert.equal(exec.deltaHo, -1) // it drops the held-out exec-cite task
    const verify = round.decisions.find(d => d.surface === "instructions.verification")
    assert.equal(verify.accepted, true)
    assert.equal(verify.deltaIn, 1)
  })
})

test("③ lineage digest chain is continuous (child.parent === digest(parent))", async () => {
  await withLineageDir(async dir => {
    const { finalManifest, seed } = await runRound(dir)
    assert.equal(finalManifest.parent, manifestDigest(seed))
    // both the seed and the promoted manifest are persisted under <scope>/<modelProfile>/<digest>.json.
    const files = readdirSync(laneOf(dir)).filter(f => f.endsWith(".json"))
    assert.ok(files.includes(`${manifestDigest(seed)}.json`))
    assert.ok(files.includes(`${manifestDigest(finalManifest)}.json`))
  })
})

test("④ rounds.jsonl has exactly one line per round, each with decisions", async () => {
  await withLineageDir(async dir => {
    await runRound(dir)
    const lines = readFileSync(path.join(laneOf(dir), "rounds.jsonl"), "utf8").split("\n").filter(Boolean)
    assert.equal(lines.length, 1)
    const rec = JSON.parse(lines[0])
    assert.equal(rec.round, 0)
    assert.equal(rec.scope, "default") // absent seed scope logged as "default" for auditability
    assert.equal(rec.decisions.length, 2)
    assert.deepEqual(rec.baseline, { heldIn: 1, heldOut: 2 })
    assert.equal(rec.proposals.length, 2)
  })
})

test("⑤ no held-out task id leaks into any prompt the model saw", async () => {
  await withLineageDir(async dir => {
    const { complete } = await runRound(dir)
    assert.equal(complete.prompts.length, 3) // mine ×2 + propose ×1
    for (const heldOutId of FIXTURE_SPLITS.heldOut) {
      for (const prompt of complete.prompts) {
        assert.ok(!prompt.includes(heldOutId), `held-out id "${heldOutId}" leaked into a prompt`)
      }
    }
  })
})

test("⑥ the non-addressable cluster is absent from the proposer's target set", async () => {
  await withLineageDir(async dir => {
    const { complete } = await runRound(dir)
    // Lock the mine call order (cluster VK first, then CEILING).
    assert.match(complete.prompts[0], /completed:VK_verify/)
    assert.match(complete.prompts[1], /max_turns:CL_solve/)
    // The proposer prompt (last call) must carry the addressable cluster but NOT the ceiling one.
    const proposePrompt = complete.prompts[2]
    assert.match(proposePrompt, /completed:VK_verify/)
    assert.ok(!proposePrompt.includes("CL_solve"), "ceiling cluster cause leaked to proposer")
    assert.ok(!proposePrompt.includes("capability ceiling"), "ceiling mechanism leaked to proposer")
  })
})

test("determinism — two identical rounds produce the same final digest", async () => {
  await withLineageDir(async dirA => {
    await withLineageDir(async dirB => {
      const a = await runRound(dirA)
      const b = await runRound(dirB)
      assert.equal(manifestDigest(a.finalManifest), manifestDigest(b.finalManifest))
    })
  })
})

// V2-S1: two scopes against ONE lineageDir must land in disjoint subtrees and never read each other's
// files — the isolation guarantee the whole slice exists for.
async function runScopedRound(lineageDir, scopeKey) {
  const complete = cannedComplete([MINE_VERIFY, MINE_CEILING, PROPOSE])
  const seed = { ...fixtureSeedManifest(), scope: scopeKey }
  const result = await selfHarnessLoop({
    seedManifest: seed,
    adapter: createFixtureAdapter(),
    heldIn: FIXTURE_SPLITS.heldIn,
    heldOut: FIXTURE_SPLITS.heldOut,
    rounds: 1,
    k: 2,
    repeats: 1,
    complete,
    lineageDir,
  })
  return { ...result, seed }
}

test("two scopes share one lineageDir but produce disjoint, non-overlapping subtrees", async () => {
  await withLineageDir(async dir => {
    const alice = await runScopedRound(dir, "alice")
    const bob = await runScopedRound(dir, "bob")

    // Each scope has its own lane; neither writes outside it.
    const aliceLane = laneOf(dir, "alice")
    const bobLane = laneOf(dir, "bob")
    // Manifest lineage files (<digest>.json); rounds.jsonl is per-lane and shares a name by design.
    const aliceFiles = readdirSync(aliceLane).filter(f => f.endsWith(".json")).sort()
    const bobFiles = readdirSync(bobLane).filter(f => f.endsWith(".json")).sort()

    // The scope rides the digest ⇒ the two seeds hash differently ⇒ zero manifest-filename overlap.
    assert.equal(alice.seed.scope, "alice")
    assert.equal(bob.seed.scope, "bob")
    assert.notEqual(manifestDigest(alice.seed), manifestDigest(bob.seed))
    assert.ok(aliceFiles.length >= 2 && bobFiles.length >= 2) // seed + promotion in each lane
    for (const f of aliceFiles) assert.ok(!bobFiles.includes(f), `manifest "${f}" appears in both scopes' lanes`)

    // rounds.jsonl in each lane records only its own scope.
    const aliceRound = JSON.parse(readFileSync(path.join(aliceLane, "rounds.jsonl"), "utf8").trim())
    const bobRound = JSON.parse(readFileSync(path.join(bobLane, "rounds.jsonl"), "utf8").trim())
    assert.equal(aliceRound.scope, "alice")
    assert.equal(bobRound.scope, "bob")

    // The top-level lineageDir holds ONLY the two scope directories — no stray flat artifacts.
    assert.deepEqual(readdirSync(dir).sort(), ["alice", "bob"])
  })
})
