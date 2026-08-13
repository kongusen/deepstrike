#!/usr/bin/env node

/**
 * WASM SDK adapter for the SPC-017 shared fixture runner.
 *
 * The adapter deliberately emits one JSON line and keeps the runner's fixture
 * comparison outside the SDK. It exercises the SDK's public exports; preparation is
 * the runner's responsibility, including linking the local WASM kernel package.
 */
import { readFileSync, realpathSync } from "node:fs"
import { dirname, isAbsolute, join, relative, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const fixturesRoot = resolve(root, "tests", "fixtures")
const resolvedFixturesRoot = realpathSync(fixturesRoot)

if (process.argv.length !== 3 || !isAbsolute(process.argv[2])) fail("usage: wasm.mjs <absolute-fixture-path>")

const sdk = await import(pathToFileURL(join(root, "wasm", "dist", "index.js")))
const stopReason = await import(pathToFileURL(join(root, "wasm", "dist", "providers", "stop-reason.js")))

class StructuredError extends Error {
  constructor(code, path, message) { super(message); this.code = code; this.path = path }
}

const fixturePath = process.argv[2]
const fixture = readJson(fixturePath)

try {
  const canonical = await project(fixture)
  emit({ ok: true, sdk: "wasm", fixture: fixture.id, canonical })
} catch (error) {
  const structured = classifyError(fixture, error)
  emit({ ok: false, sdk: "wasm", fixture: fixture.id, error: structured })
}

async function project(value) {
  switch (value.domain) {
    case "agent_ir": {
      const raw = readReferenced(value.input.fixture)
      const spec = sdk.lowerAgent(sdk.normalizeAgent(raw))
      return {
        name: spec.name,
        ...(spec.capabilityFilter ? { capabilityFilter: spec.capabilityFilter } : {}),
        effectiveCapabilities: spec.effectiveCapabilities,
      }
    }
    case "durable_tool_result": {
      const input = value.input.fixture ? readReferenced(value.input.fixture) : value.input.value
      const result = sdk.decodeDurableToolResult(input)
      return {
        call_id: result.call_id,
        is_error: result.is_error,
        blockTypes: result.blocks.map(block => block.type),
      }
    }
    case "prompt_measurement": {
      const measurement = value.input.value
      if (!measurement || typeof measurement !== "object" || Array.isArray(measurement)) {
        throw new Error("prompt measurement must be an object")
      }
      const recorded = sdk.recordPromptMeasurement(
        { fingerprint: measurement.requestFingerprint },
        {
          inputTokens: measurement.inputTokens,
          source: measurement.source,
          confidence: measurement.confidence,
        },
      )
      return clone(recorded)
    }
    case "provider_error": {
      try {
        return { stopReason: stopReason.decodeCanonicalStopReason(value.input.stopReason) }
      } catch (error) {
        throw new StructuredError("unknown_stop_reason", "/stopReason", String(error?.message ?? error))
      }
    }
    case "session_event": {
      const event = value.input.event
      if (!event || typeof event !== "object" || Array.isArray(event)) throw new Error("session event must be an object")
      if (event.kind !== "tool_completed") throw new Error(`unsupported session event kind: ${String(event.kind)}`)
      const content = sdk.decodeDurableContent(event.content)
      const log = new sdk.InMemorySessionLog()
      await log.append("spc-017", {
        kind: "tool_completed",
        turn: 0,
        results: [{
          call_id: requiredString(event.callId, "/callId"),
          output: "",
          is_error: requiredBoolean(event.isError, "/isError"),
          content,
        }],
      })
      const [entry] = await log.read("spc-017")
      if (!entry || entry.event.kind !== "tool_completed") {
        throw new StructuredError("invalid_session_event", "/event", "session event did not replay as tool_completed")
      }
      const [result] = entry.event.results
      const recordedContent = sdk.decodeDurableContent(result.content)
      return {
        kind: entry.event.kind,
        callId: result.call_id,
        isError: result.is_error ?? false,
        blockTypes: recordedContent.blocks.map(block => block.type),
      }
    }
    case "provider_request_plan": {
      const request = readReferenced(value.input.fixture)
      return { fingerprint: sdk.createProviderRequestPlan(request.input).fingerprint }
    }
    default:
      throw new StructuredError("unsupported_domain", "/domain", `unsupported domain: ${String(value.domain)}`)
  }
}

function readReferenced(reference) {
  if (typeof reference !== "string" || !reference
    || isAbsolute(reference) || /^[\\/]/.test(reference) || /^[a-zA-Z]:[\\/]/.test(reference)
    || reference.split(/[\\/]+/).includes("..")) {
    throw new StructuredError("invalid_fixture_reference", "/input/fixture", "fixture reference must be a relative path under tests/fixtures")
  }
  const candidate = resolve(fixturesRoot, reference)
  if (!isWithin(fixturesRoot, candidate)) {
    throw new StructuredError("invalid_fixture_reference", "/input/fixture", "fixture reference must stay under tests/fixtures")
  }
  try {
    const path = realpathSync(candidate)
    if (!isWithin(resolvedFixturesRoot, path)) {
      throw new StructuredError("invalid_fixture_reference", "/input/fixture", "fixture reference must stay under tests/fixtures")
    }
    return readJson(path)
  } catch (error) {
    if (error instanceof StructuredError) throw error
    throw new StructuredError("invalid_fixture_reference", "/input/fixture", "fixture reference must resolve under tests/fixtures")
  }
}
function isWithin(root, candidate) {
  const path = relative(root, candidate)
  return path !== "" && path !== ".." && !path.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`)
    && !isAbsolute(path)
}
function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"))
}
function requiredString(value, path) {
  if (typeof value !== "string" || !value) throw new StructuredError("invalid_session_event", path, "expected non-empty string")
  return value
}
function requiredBoolean(value, path) {
  if (typeof value !== "boolean") throw new StructuredError("invalid_session_event", path, "expected boolean")
  return value
}
function clone(value) {
  if (value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone)
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clone(item)]))
}
function classifyError(fixture, error) {
  if (error instanceof StructuredError) return { code: error.code, path: error.path, message: error.message }
  if (fixture.domain === "durable_tool_result") {
    const message = String(error?.message ?? error)
    const path = message.includes("is_error") ? "/is_error" : ""
    return { code: "invalid_durable_tool_result", path, message }
  }
  return { code: "adapter_failure", path: "", message: String(error?.message ?? error) }
}
function emit(value) { process.stdout.write(`${JSON.stringify(value)}\n`) }
function fail(message) {
  emit({ ok: false, sdk: "wasm", fixture: "", error: { code: "adapter_failure", path: "", message } })
  process.exit(2)
}
