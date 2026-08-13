import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { resolve } from "node:path"
import { mkdtempSync, readFileSync, rmSync, symlinkSync, unlinkSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import test from "node:test"

function run(fixture) {
  return spawnSync(process.execPath, ["scripts/sdk-conformance/wasm.mjs", resolve(`tests/fixtures/sdk-conformance/canonical/${fixture}.json`)], { encoding: "utf8" })
}

test("WASM adapter projects Agent IR and durable tool result", () => {
  const agent = run("agent-ir-basic")
  assert.equal(agent.status, 0, agent.stderr)
  assert.equal(JSON.parse(agent.stdout).canonical.name, "researcher")
  const durable = run("durable-tool-result")
  assert.equal(durable.status, 0, durable.stderr)
  assert.deepEqual(JSON.parse(durable.stdout).canonical.blockTypes, ["text", "image", "file", "video"])
})

test("WASM adapter emits structured durable validation errors", () => {
  const result = run("durable-tool-result-invalid-is-error")
  assert.equal(result.status, 0, result.stderr)
  const envelope = JSON.parse(result.stdout)
  assert.equal(envelope.ok, false)
  assert.equal(envelope.error.code, "invalid_durable_tool_result")
  assert.equal(envelope.error.path, "/is_error")
})

test("WASM adapter matches request plan, measurement, session event, and provider error fixtures", () => {
  const plan = run("provider-request-plan")
  assert.equal(plan.status, 0, plan.stderr)
  assert.match(JSON.parse(plan.stdout).canonical.fingerprint, /^sha256:[0-9a-f]{64}$/)
  const measurement = run("prompt-measurement")
  assert.equal(measurement.status, 0, measurement.stderr)
  assert.equal(JSON.parse(measurement.stdout).canonical.inputTokens, 12)
  const event = run("session-event-tool-completed")
  assert.equal(event.status, 0, event.stderr)
  assert.deepEqual(JSON.parse(event.stdout).canonical.blockTypes, ["text"])
  const providerError = run("provider-error-unknown-stop")
  assert.equal(providerError.status, 0, providerError.stderr)
  assert.equal(JSON.parse(providerError.stdout).error.code, "unknown_stop_reason")
})

test("WASM adapter requires one absolute fixture path", () => {
  const result = spawnSync(process.execPath, ["scripts/sdk-conformance/wasm.mjs", "tests/fixtures/sdk-conformance/canonical/agent-ir-basic.json"], { encoding: "utf8" })
  assert.notEqual(result.status, 0)
  assert.match(result.stdout, /absolute-fixture-path/)

  const extra = spawnSync(process.execPath, [
    "scripts/sdk-conformance/wasm.mjs",
    resolve("tests/fixtures/sdk-conformance/canonical/agent-ir-basic.json"),
    "unexpected",
  ], { encoding: "utf8" })
  assert.notEqual(extra.status, 0)
  assert.match(extra.stdout, /absolute-fixture-path/)
  assert.equal(extra.stdout.trim().split("\n").length, 1)
})

test("WASM adapter projects from the SDK public entry point", () => {
  const result = spawnSync(process.execPath, ["scripts/sdk-conformance/wasm.mjs", resolve("tests/fixtures/sdk-conformance/canonical/prompt-measurement.json")], { encoding: "utf8" })
  assert.equal(result.status, 0, result.stderr)
  assert.deepEqual(JSON.parse(result.stdout).canonical, {
    requestFingerprint: "sha256:fixture",
    inputTokens: 12,
    source: { kind: "heuristic" },
    confidence: "low_confidence",
  })
})

test("WASM adapter rejects fixture references outside tests/fixtures", () => {
  const path = resolve(tmpdir(), `deepstrike-wasm-conformance-${process.pid}.json`)
  try {
    writeFileSync(path, JSON.stringify({
      id: "outside-fixture-reference",
      domain: "agent_ir",
      input: { fixture: "../sdk-conformance/canonical/agent-ir-basic.json" },
      expected: { error: { code: "invalid_fixture_reference", path: "/input/fixture" } },
    }))
    const result = spawnSync(process.execPath, ["scripts/sdk-conformance/wasm.mjs", path], { encoding: "utf8" })
    assert.equal(result.status, 0, result.stderr)
    const envelope = JSON.parse(result.stdout)
    assert.equal(envelope.ok, false)
    assert.equal(envelope.error.code, "invalid_fixture_reference")
    assert.equal(envelope.error.path, "/input/fixture")
  } finally {
    rmSync(path, { force: true })
  }
})

test("WASM adapter rejects in-bound traversal and symlink fixture references", () => {
  const path = resolve(tmpdir(), `deepstrike-wasm-conformance-${process.pid}.json`)
  const fixture = (reference) => ({
    id: "outside-fixture-reference",
    domain: "agent_ir",
    input: { fixture: reference },
    expected: { error: { code: "invalid_fixture_reference", path: "/input/fixture" } },
  })
  const runFixture = reference => {
    writeFileSync(path, JSON.stringify(fixture(reference)))
    return JSON.parse(spawnSync(process.execPath, ["scripts/sdk-conformance/wasm.mjs", path], { encoding: "utf8" }).stdout)
  }
  const source = mkdtempSync(resolve(tmpdir(), "deepstrike-wasm-conformance-source-"))
  const link = resolve("tests", "fixtures", `.sdk-conformance-escape-${process.pid}-${Date.now()}.json`)
  try {
    assert.equal(runFixture("agent-ir/../agent-ir/canonical-agent.json").error.code, "invalid_fixture_reference")
    writeFileSync(resolve(source, "agent.json"), readFileSync(resolve("tests/fixtures/agent-ir/canonical-agent.json")))
    symlinkSync(resolve(source, "agent.json"), link)
    assert.equal(runFixture(link.slice(resolve("tests/fixtures").length + 1)).error.code, "invalid_fixture_reference")
  } finally {
    unlinkSync(link)
    rmSync(path, { force: true })
    rmSync(source, { recursive: true, force: true })
  }
})
