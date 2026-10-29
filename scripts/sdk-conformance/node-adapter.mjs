#!/usr/bin/env node
import { readFile, mkdtemp, realpath } from "node:fs/promises"
import { tmpdir } from "node:os"
import { fileURLToPath } from "node:url"
import { dirname, isAbsolute, join, relative, resolve } from "node:path"
import {
  decodeDurableToolResult,
  decodeDurableContent,
  decodeCanonicalContentParts,
  encodeCanonicalContentParts,
  SESSION_EVENT_REGISTRY,
  kernelObservationToSessionEvent,
  readSessionEvidence,
} from "../../node/dist/conformance.js"
import { createNativeContextPreparationAdapter, providerAttemptToRecord } from "../../node/dist/runtime/public.js"
import { createProviderRequestPlan, recordPromptMeasurement } from "../../node/dist/providers/public.js"
import { FileSessionLog, InMemorySessionLog } from "../../node/dist/advanced/public.js"

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
    case "context_execution": {
      const adapter = createNativeContextPreparationAdapter()
      const prepared = adapter.prepare(input.request)
      return {
        input_digest: prepared.execution_input.input_digest,
        plan_digest: prepared.plan.plan_id,
        verified: adapter.verify(input.request.effect, prepared),
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
    case "content_parts_v1": {
      // F14/B5 byte contract: encode pins exact bytes; decode of an unknown prefix or an
      // undecodable payload must return nothing (the text stays literal — never guess).
      if (Array.isArray(input.parts)) {
        const encoded = encodeCanonicalContentParts(input.parts)
        const decoded = decodeCanonicalContentParts(encoded)
        return { encoded, roundtrip: JSON.stringify(decoded) === JSON.stringify(input.parts) }
      }
      if (typeof input.decode === "string") {
        return { decoded: decodeCanonicalContentParts(input.decode) ?? null }
      }
      invalid("invalid_content_parts", "/input", "content_parts_v1 input must carry parts or decode")
    }
    case "provider_attempt_record": {
      // P4 §3 (0.2.64 S3): the attempt record is pinned in its canonical JSON form — snake_case
      // top-level fields (the wire convention in every SDK), nested objects in the TS-native
      // camelCase spelling (the wire-family convention the provider_request_plan fingerprint
      // already pins). `attempt` exercises the SDK's record builder; `record` roundtrips a wire
      // record through the durable codec, whose read path carries the validation teeth (G2).
      if (input.attempt) {
        return projectAttemptRecord(providerAttemptToRecord(input.attempt, input.policyId))
      }
      if (input.record) {
        const log = new FileSessionLog(await mkdtemp(join(tmpdir(), "ds-conf-")))
        await log.append("spc-017", { kind: "provider_attempt", ...input.record })
        const [entry] = await log.read("spc-017")
        return projectAttemptRecord(entry.event)
      }
      invalid("invalid_provider_attempt", "/input", "provider_attempt_record input must carry attempt or record")
    }
    case "session_event_vocabulary": {
      // The registry-backed vocabulary (kinds + top-level fields), compile-time checked against
      // the SessionEvent union. The runner compares it with this SDK's slice of the registry.
      const kinds = {}
      for (const kind of Object.keys(SESSION_EVENT_REGISTRY).sort()) {
        kinds[kind] = { required: [...SESSION_EVENT_REGISTRY[kind].required], optional: [...SESSION_EVENT_REGISTRY[kind].optional] }
      }
      return { kinds }
    }
    case "shared_evidence_snapshot": {
      const snapshots = {}
      for (const item of input.cases) {
        const sessionLog = new InMemorySessionLog()
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
        const snapshot = await readSessionEvidence({
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
      for (const item of input.cases) {
        const options = item.options ?? {}
        const event = kernelObservationToSessionEvent(item.observation, item.turn, {
          nextArchiveStart: options.next_archive_start,
          latestSeq: options.latest_seq,
          preservedRefs: options.preserved_refs,
        })
        events[item.name] = event === null ? null : JSON.parse(JSON.stringify(event))
      }
      return { events }
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

/** The SDK-neutral (snake_case) projection the shared_evidence_snapshot fixture pins. Diagnostic
 *  messages are free text and stay out of the contract; codes, domains and ref ids are pinned. */
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
  // Canonical comparison form (0.2.64 S3): snake_case top level, nested objects verbatim
  // (camelCase wire-family spelling). Only the pinned field set survives — `kind` is the
  // SessionLog envelope, not part of the record.
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

async function main() {
  if (process.argv.length !== 3) throw new Error("usage: node-adapter.mjs <fixture.json>")
  if (!isAbsolute(process.argv[2])) throw new Error("fixture path must be absolute")
  const fixturePath = resolve(process.argv[2])
  const fixture = JSON.parse(await readFile(fixturePath, "utf8"))
  const base = {
    sdk: "node",
    fixture: fixture.id,
  }
  try {
    const canonical = await canonicalFor(fixture)
    process.stdout.write(`${JSON.stringify({ ok: true, ...base, canonical })}\n`)
  } catch (error) {
    const durableError = fixture.domain === "durable_tool_result"
      ? { code: "invalid_durable_tool_result", path: String(error?.message ?? "").includes("is_error") ? "/is_error" : "" }
      : undefined
    const attemptError = fixture.domain === "provider_attempt_record"
      ? {
          code: "invalid_provider_attempt",
          // Native messages name the rejected field: "provider_attempt effect_id is required".
          path: `/${String(error?.message ?? "").match(/provider_attempt (\w+)/)?.[1] ?? ""}`,
        }
      : undefined
    const code = error?.code ?? durableError?.code ?? attemptError?.code ?? "conformance_error"
    const path = error?.path ?? durableError?.path ?? attemptError?.path ?? ""
    const message = error instanceof Error ? error.message : String(error)
    process.stdout.write(`${JSON.stringify({ ok: false, ...base, error: { code, path, message } })}\n`)
  }
}

await main()
