#!/usr/bin/env node
import { readFile, realpath } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { dirname, isAbsolute, join, relative, resolve } from "node:path"
import {
  createProviderRequestPlan,
  decodeDurableToolResult,
  decodeDurableContent,
  InMemorySessionLog,
  lowerAgent,
  normalizeAgent,
  recordPromptMeasurement,
} from "../../node/dist/index.js"

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..")
const FIXTURES_ROOT = resolve(ROOT, "tests", "fixtures")
const STOP_REASONS = new Set(["end_turn", "tool_use", "max_tokens", "stop_sequence", "content_filter", "other"])

function invalid(code, path, message) {
  const error = new Error(message)
  error.code = code
  error.path = path
  throw error
}

function isWithin(root, candidate) {
  const path = relative(root, candidate)
  return path !== "" && !path.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`)
    && path !== ".." && !isAbsolute(path)
}

async function inputFixturePath(relativePath) {
  if (typeof relativePath !== "string" || !relativePath
    || isAbsolute(relativePath) || /^[\\/]/.test(relativePath) || /^[a-zA-Z]:[\\/]/.test(relativePath)
    || relativePath.split(/[\\/]+/).includes("..")) {
    invalid("invalid_fixture_reference", "/input/fixture", "fixture reference must be a relative path under tests/fixtures")
  }
  const candidate = resolve(FIXTURES_ROOT, relativePath)
  if (!isWithin(FIXTURES_ROOT, candidate)) {
    invalid("invalid_fixture_reference", "/input/fixture", "fixture reference must stay under tests/fixtures")
  }
  try {
    const [resolvedRoot, resolvedCandidate] = await Promise.all([realpath(FIXTURES_ROOT), realpath(candidate)])
    if (!isWithin(resolvedRoot, resolvedCandidate)) {
      invalid("invalid_fixture_reference", "/input/fixture", "fixture reference must stay under tests/fixtures")
    }
    return resolvedCandidate
  } catch (error) {
    if (error?.code === "invalid_fixture_reference") throw error
    invalid("invalid_fixture_reference", "/input/fixture", "fixture reference must resolve under tests/fixtures")
  }
}

async function canonicalFor(fixture) {
  const input = fixture.input ?? {}
  switch (fixture.domain) {
    case "agent_ir": {
      const source = JSON.parse(await readFile(await inputFixturePath(input.fixture), "utf8"))
      const lowered = lowerAgent(normalizeAgent(source))
      return {
        version: lowered.version,
        name: lowered.name,
        ...(lowered.capabilityFilter ? { capabilityFilter: lowered.capabilityFilter } : {}),
        effectiveCapabilities: lowered.effectiveCapabilities,
      }
    }
    case "provider_request_plan": {
      const source = JSON.parse(await readFile(await inputFixturePath(input.fixture), "utf8"))
      const plan = createProviderRequestPlan(source.input)
      return { fingerprint: plan.fingerprint }
    }
    case "durable_tool_result": {
      const value = input.fixture
        ? JSON.parse(await readFile(await inputFixturePath(input.fixture), "utf8"))
        : input.value
      const result = decodeDurableToolResult(value)
      return {
        schema_version: result.schema_version,
        call_id: result.call_id,
        is_error: result.is_error,
        blockTypes: result.blocks.map(block => block.type),
      }
    }
    case "prompt_measurement": {
      const value = input.value ?? {}
      return recordPromptMeasurement(
        { fingerprint: value.requestFingerprint },
        {
          inputTokens: value.inputTokens,
          source: value.source,
          confidence: value.confidence,
        },
      )
    }
    case "provider_error": {
      const stopReason = input.stopReason
      if (typeof stopReason !== "string" || !STOP_REASONS.has(stopReason)) {
        invalid("unknown_stop_reason", "/stopReason", `unknown stop reason: ${String(stopReason)}`)
      }
      return { stopReason }
    }
    case "session_event": {
      const event = input.event ?? {}
      const content = decodeDurableContent(event.content)
      const sessionLog = new InMemorySessionLog()
      await sessionLog.append("spc-017", {
        kind: "tool_completed",
        turn: 0,
        results: [{
          call_id: event.callId,
          output: "",
          is_error: event.isError,
          content,
        }],
      })
      const [entry] = await sessionLog.read("spc-017")
      if (!entry || entry.event.kind !== "tool_completed") {
        invalid("invalid_session_event", "/event", "session event did not replay as tool_completed")
      }
      const [result] = entry.event.results
      const recordedContent = decodeDurableContent(result.content)
      return {
        kind: entry.event.kind,
        callId: result.call_id,
        isError: result.is_error ?? false,
        blockTypes: recordedContent.blocks.map(block => block.type),
      }
    }
    default:
      invalid("unsupported_domain", "/domain", `unsupported conformance domain: ${String(fixture.domain)}`)
  }
}

async function main() {
  if (process.argv.length !== 3) throw new Error("usage: node-adapter.mjs <fixture.json>")
  if (!isAbsolute(process.argv[2])) throw new Error("fixture path must be absolute")
  const fixturePath = resolve(process.argv[2])
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"))
  const base = {
    sdk: "node",
    fixture: fixture.id,
    contractVersion: fixture.expected?.contractVersion,
  }
  try {
    const canonical = await canonicalFor(fixture)
    process.stdout.write(`${JSON.stringify({ ok: true, ...base, canonical })}\n`)
  } catch (error) {
    const durableError = fixture.domain === "durable_tool_result"
      ? { code: "invalid_durable_tool_result", path: String(error?.message ?? "").includes("is_error") ? "/is_error" : "" }
      : undefined
    const code = error?.code ?? durableError?.code ?? "conformance_error"
    const path = error?.path ?? durableError?.path ?? ""
    const message = error instanceof Error ? error.message : String(error)
    process.stdout.write(`${JSON.stringify({ ok: false, ...base, error: { code, path, message } })}\n`)
  }
}

await main()
