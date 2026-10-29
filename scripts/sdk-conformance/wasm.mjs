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
const registry = await import(pathToFileURL(join(root, "wasm", "dist", "runtime", "session-event-registry.generated.js")))
const eventLog = await import(pathToFileURL(join(root, "wasm", "dist", "runtime", "kernel-event-log.js")))

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
    case "context_execution": {
      const adapter = await sdk.createNativeContextPreparationAdapter()
      const prepared = adapter.prepare(value.input.request)
      return {
        input_digest: prepared.execution_input.input_digest,
        plan_digest: prepared.plan.plan_id,
        verified: adapter.verify(value.input.request.effect, prepared),
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
    case "content_parts_v1": {
      // F14/B5 byte contract: encode pins exact bytes; decode of an unknown prefix or an
      // undecodable payload must return nothing (the text stays literal — never guess).
      if (Array.isArray(value.input.parts)) {
        const encoded = sdk.encodeCanonicalContentParts(value.input.parts)
        const decoded = sdk.decodeCanonicalContentParts(encoded)
        return { encoded, roundtrip: JSON.stringify(decoded) === JSON.stringify(value.input.parts) }
      }
      if (typeof value.input.decode === "string") {
        return { decoded: sdk.decodeCanonicalContentParts(value.input.decode) ?? null }
      }
      throw new StructuredError("invalid_content_parts", "/input", "content_parts_v1 input must carry parts or decode")
    }
    case "provider_error": {
      try {
        return { stopReason: stopReason.decodeCanonicalStopReason(value.input.stopReason) }
      } catch (error) {
        throw new StructuredError("unknown_stop_reason", "/stopReason", String(error?.message ?? error))
      }
    }
    case "provider_attempt_record": {
      // P4 §3 (0.2.64 S3): canonical JSON form — snake_case top level, nested objects verbatim
      // (camelCase wire-family spelling, same as provider_request_plan). `attempt` exercises the
      // SDK record builder; `record` runs the exported validator (wasm's teeth live there — the
      // in-memory SessionLog has no durable read path) then roundtrips through the log.
      if (value.input.attempt) {
        return projectAttemptRecord(sdk.providerAttemptToRecord(value.input.attempt, value.input.policyId))
      }
      if (value.input.record) {
        const record = { kind: "provider_attempt", ...value.input.record }
        assertAttemptRecord(record)
        const log = new sdk.InMemorySessionLog()
        await log.append("spc-017", record)
        const [entry] = await log.read("spc-017")
        return projectAttemptRecord(entry.event)
      }
      throw new StructuredError("invalid_provider_attempt", "/input", "provider_attempt_record input must carry attempt or record")
    }
    case "session_event_vocabulary": {
      // The registry-backed vocabulary (kinds + top-level fields), compile-time checked against
      // the SessionEvent union. The runner compares it with this SDK's slice of the registry.
      const kinds = {}
      for (const kind of Object.keys(registry.SESSION_EVENT_REGISTRY).sort()) {
        kinds[kind] = { required: [...registry.SESSION_EVENT_REGISTRY[kind].required], optional: [...registry.SESSION_EVENT_REGISTRY[kind].optional] }
      }
      return { kinds }
    }
    case "shared_evidence_snapshot": {
      const snapshots = {}
      for (const item of value.input.cases) {
        const sessionLog = new sdk.InMemorySessionLog()
        for (const event of item.events) await sessionLog.append(item.session_id, event)
        const journal = item.journal
        const kernelJournal = journal === undefined ? undefined : {
          readFrom: async () => journal.entries.map(entry => ({
            step_seq: entry.step_seq, record_digest: entry.record_digest, record_bytes: Uint8Array.from(entry.record_bytes),
          })),
          latestCheckpoint: async () => journal.checkpoint === undefined ? undefined : {
            ...journal.checkpoint, checkpoint_bytes: Uint8Array.from(journal.checkpoint.checkpoint_bytes),
          },
        }
        const sidecar = item.sidecar === undefined ? undefined : {
          knowledgeProvenance: ({ entryId }) => item.sidecar.knowledge_provenance?.[entryId],
          memoryCandidateReceipt: ({ candidateId }) => item.sidecar.memory_candidate_receipts?.[candidateId],
        }
        const scope = item.scope ?? {}
        const snapshot = await sdk.readSessionEvidence({
          operationId: item.operation_id,
          sessionId: item.session_id,
          scope: {
            ...(scope.agent_id === undefined ? {} : { agentId: scope.agent_id }),
            ...(scope.tenant_id === undefined ? {} : { tenantId: scope.tenant_id }),
            ...(scope.namespace === undefined ? {} : { namespace: scope.namespace }),
          },
          sessionLog,
          ...(kernelJournal === undefined ? {} : { kernelJournal }),
          ...(sidecar === undefined ? {} : { sidecar }),
        })
        snapshots[item.name] = projectEvidenceSnapshot(snapshot)
      }
      return { snapshots }
    }
    case "kernel_observation_projection": {
      const events = {}
      for (const item of value.input.cases) {
        const options = item.options ?? {}
        const event = eventLog.kernelObservationToSessionEvent(item.observation, item.turn, {
          nextArchiveStart: options.next_archive_start,
          latestSeq: options.latest_seq,
          preservedRefs: options.preserved_refs,
        })
        events[item.name] = event === null ? null : JSON.parse(JSON.stringify(event))
      }
      return { events }
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

function projectEvidenceSnapshot(snapshot) {
  const defined = object => Object.fromEntries(Object.entries(object).filter(([, value]) => value !== undefined))
  const asOf = snapshot.asOf ?? {}
  return {
    as_of: defined({
      session_log_seq: asOf.sessionLogSeq, journal_step_seq: asOf.journalStepSeq, checkpoint_ordinal: asOf.checkpointOrdinal,
    }),
    completeness: snapshot.completeness,
    diagnostics: snapshot.diagnostics.map(item => defined({ code: item.code, domain: item.domain, ref_id: item.refId })),
    refs: snapshot.refs.map(ref => defined({
      domain: ref.domain,
      ref_id: ref.refId,
      authority: ref.authority,
      availability: ref.availability,
      content_digest: ref.contentDigest,
      payload_ref: ref.payloadRef,
      size_bytes: ref.sizeBytes,
      version: ref.version,
      provenance: ref.provenance,
      settlement: ref.settlement,
      evidence_refs: ref.evidenceRefs === undefined ? undefined : [...ref.evidenceRefs],
      links: ref.links?.map(link => ({ relation: link.relation, domain: link.domain, ref_id: link.refId })),
      retention: ref.retention === undefined ? undefined : defined({
        pinned: ref.retention.pinned, observed_ttl_days: ref.retention.observedTtlDays, prunable: ref.retention.prunable,
      }),
    })),
    verification: {
      journal: snapshot.verification.journal.length,
      session_logs: (snapshot.verification.sessionLogs ?? []).map(stream => stream.length),
      checkpoints: (snapshot.verification.checkpoints ?? []).length,
    },
  }
}

function projectAttemptRecord(record) {
  // Canonical comparison form (0.2.64 S3): snake_case top level, nested objects verbatim.
  return {
    effect_id: record.effect_id,
    attempt_seq: record.attempt_seq,
    route: record.route,
    request_fingerprint: record.request_fingerprint,
    status: record.status,
    transport_rungs: record.transport_rungs,
    ...(record.last_error_class !== undefined ? { last_error_class: record.last_error_class } : {}),
    started_at_ms: record.started_at_ms,
    finished_at_ms: record.finished_at_ms,
    ...(record.usage !== undefined ? { usage: record.usage } : {}),
    ...(record.wire_evidence !== undefined ? { wire_evidence: record.wire_evidence } : {}),
    ...(record.accounting_policy_id !== undefined ? { accounting_policy_id: record.accounting_policy_id } : {}),
  }
}

function assertAttemptRecord(record) {
  try {
    sdk.assertCanonicalProviderAttemptRecord(record)
  } catch (error) {
    // Native messages name the rejected field: "provider_attempt effect_id is required".
    const field = String(error?.message ?? "").match(/provider_attempt (\w+)/)?.[1] ?? ""
    throw new StructuredError("invalid_provider_attempt", `/${field}`, String(error?.message ?? error))
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
