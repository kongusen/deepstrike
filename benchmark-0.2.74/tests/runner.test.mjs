import test from "node:test"
import assert from "node:assert/strict"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { diffMetrics } from "../core/metrics.mjs"
import { compareScenario, runScenario, saveBaseline, checkBaseline } from "../core/runner.mjs"

test("metric diff compares nested metric values", () => {
  const diff = diffMetrics({ a: { value: 1, unit: "count" } }, { a: { value: 3, unit: "count" } })
  assert.deepEqual(diff[0], { key: "a", left: 1, right: 3, delta: 2, unit: "count", changed: true })
})

test("contract and behavior scenarios emit passed artifacts", async () => {
  const contract = await runScenario("contract-surface")
  const behavior = await runScenario("planes-harness-evals")
  assert.equal(contract.status, "passed")
  assert.equal(behavior.status, "passed")
  assert.equal(behavior.sdkVersion, "0.2.74")
})

test("workflow compare runs both public API variants", async () => {
  const result = await compareScenario("workflow")
  assert.equal(result.artifacts.length, 2)
  assert.ok(result.artifacts.every(artifact => artifact.status === "passed"))
  assert.ok(result.comparisons[0].metricDiff.every(diff => !diff.changed))
})

test("baseline save/check detects deterministic artifact equality", async () => {
  const root = await mkdtemp(join(tmpdir(), "deepstrike-benchmark-"))
  try {
    const artifact = await runScenario("contract-surface")
    await saveBaseline(root, artifact)
    const check = await checkBaseline(root, artifact)
    assert.equal(check.passed, true)
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})
