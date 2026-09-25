import test from "node:test"
import assert from "node:assert/strict"
import { loadSdk, assertSdkVersion, EXPECTED_SDK_VERSION } from "../core/sdk.mjs"
import { assertPublicContracts, checkPublicContracts, checkPackageExportMap, PUBLIC_CONTRACTS } from "../contracts/manifest.mjs"

test("loads the 0.2.74 package export map", async () => {
  const sdk = assertSdkVersion(await loadSdk())
  assert.equal(sdk.version, EXPECTED_SDK_VERSION)
  assert.deepEqual(Object.keys(sdk.surfaces).sort(), ["advanced", "evals", "harness", "memory", "os", "planes", "providers", "root", "runtime", "workflow"])
})

test("all declared public barrel contracts pass", async () => {
  const sdk = await loadSdk()
  const report = assertPublicContracts(sdk)
  assert.equal(report.results.length, PUBLIC_CONTRACTS.length + 1)
  assert.ok(report.results.every(result => result.passed))
})

test("the package export map keeps import and declaration entry points aligned", async () => {
  const sdk = await loadSdk()
  assert.equal(checkPackageExportMap(sdk).passed, true)
})

test("contract report identifies drift without hiding missing or leaked symbols", async () => {
  const sdk = await loadSdk()
  const clone = { ...sdk, surfaces: { ...sdk.surfaces, root: { ...sdk.root, RuntimeRunner: class {} } } }
  const report = checkPublicContracts(clone)
  assert.equal(report.passed, false)
  assert.ok(report.failures.some(f => f.id === "root.intent" && f.forbiddenPresent.includes("RuntimeRunner")))
})
