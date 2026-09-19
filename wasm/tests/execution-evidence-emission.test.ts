/**
 * P4-S2 (0.2.64 Execution Evidence Plane): wasm runner emission parity with node/python.
 *
 * One real run must land the full chain in SessionLog:
 * run_started.route → prompt_measured.effect_id → provider_attempt (one per terminal exit:
 * success / transport_exhausted / aborted / rejected) → llm_completed(effect_id, invocation_id,
 * wire_evidence). Everything asserted here is L2 host evidence (B7) — never kernel input.
 *
 * The wasm SessionLog is in-memory only, so the python FileSessionLog roundtrip/forgery tests
 * have no wasm counterpart; shape enforcement for the wire record lives in the node/python
 * session-log validators and the S3 conformance fixtures.
 */
import {
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
} from "../src/runtime/index.js"
import type { SessionEvent } from "../src/runtime/session-log.js"
import {
  FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY,
  providerAttemptToRecord,
  tryNormalizeProviderUsage,
} from "../src/runtime/execution-evidence.js"
import type { LLMProvider, ProviderMessage, StreamEvent } from "../src/types.js"

/** Answers turn 1 with a usage frame + text, then ends the run. */
const oneTurnProvider = (): LLMProvider => ({
  async complete(): Promise<ProviderMessage> {
    throw new Error("not implemented")
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "usage", totalTokens: 30, inputTokens: 20, outputTokens: 10 }
    yield { type: "text_delta", delta: "done" }
  },
})

const failingProvider = (): LLMProvider => ({
  async complete(): Promise<ProviderMessage> {
    throw new Error("not implemented")
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "partial" }
    throw new Error("boom")
  },
})

/** A native-exact measurement far over budget → rejected before any transport. */
const oversizedPromptProvider = (): LLMProvider => ({
  async complete(): Promise<ProviderMessage> {
    throw new Error("not implemented")
  },
  async countTokens() {
    return { inputTokens: 10 ** 9, source: { kind: "native", provider: "oversized" }, confidence: "exact" as const }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "never reached" }
  },
})

const longStreamProvider = (): LLMProvider => ({
  async complete(): Promise<ProviderMessage> {
    throw new Error("not implemented")
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "first" }
    for (let i = 0; i < 1000; i += 1) yield { type: "text_delta", delta: "later" }
  },
})

const eventsOf = async (log: InMemorySessionLog, sessionId: string): Promise<SessionEvent[]> =>
  (await log.read(sessionId)).map(entry => entry.event)

const kindsOf = <K extends SessionEvent["kind"]>(events: SessionEvent[], kind: K) =>
  events.filter(event => event.kind === kind) as Array<Extract<SessionEvent, { kind: K }>>

describe("execution-evidence emission (P4)", () => {
  it("a successful turn lands the full evidence chain", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: oneTurnProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })

    for await (const _event of runner.run({ sessionId: "evidence-success", goal: "say done" })) {
      // drain
    }

    const events = await eventsOf(sessionLog, "evidence-success")

    const started = kindsOf(events, "run_started")
    expect(started).toHaveLength(1)
    const route = started[0].route
    expect(route?.routeId.startsWith("sha256:")).toBe(true)
    expect(route?.endpoint.id).toBeTruthy()

    const measured = kindsOf(events, "prompt_measured")
    expect(measured.length).toBeGreaterThan(0)
    for (const m of measured) expect(m.effect_id).toBeTruthy()
    const fingerprint = measured[0].measurement.requestFingerprint

    const attempts = kindsOf(events, "provider_attempt")
    expect(attempts).toHaveLength(1)
    const attempt = attempts[0]
    expect(attempt.status).toBe("success")
    expect(attempt.effect_id).toBe(measured[0].effect_id)
    expect(attempt.request_fingerprint).toBe(fingerprint)
    expect(attempt.route.routeId).toBe(route?.routeId)
    expect(attempt.attempt_seq).toBe(1)
    expect(attempt.transport_rungs).toBe(1)
    // The policy id pins only because a measurement exists (P4 §2.1).
    expect(attempt.accounting_policy_id).toBe(FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY.policyId)
    expect(attempt.usage?.inputTokens).toBe(20)
    expect(attempt.usage?.outputTokens).toBe(10)
    expect(attempt.wire_evidence?.request_fingerprint).toBe(fingerprint)
    expect(attempt.started_at_ms).toBeLessThanOrEqual(attempt.finished_at_ms)

    const completed = kindsOf(events, "llm_completed")
    expect(completed).toHaveLength(1)
    expect(completed[0].effect_id).toBe(attempt.effect_id)
    // First-try success: the invocation id IS the chain's only effect id (P4 §1.1).
    expect(completed[0].invocation_id).toBe(attempt.effect_id)
    expect(completed[0].wire_evidence?.request_fingerprint).toBe(fingerprint)
  })

  it("a transport exhaustion lands a failed attempt with the error CLASS only", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: failingProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })

    for await (const _event of runner.run({ sessionId: "evidence-transport", goal: "explode" })) {
      // drain
    }

    const events = await eventsOf(sessionLog, "evidence-transport")
    const attempts = kindsOf(events, "provider_attempt")
    expect(attempts).toHaveLength(1)
    expect(attempts[0].status).toBe("transport_exhausted")
    expect(attempts[0].transport_rungs).toBe(1)
    // The error CLASS only — never the raw vendor text (B1).
    expect(attempts[0].last_error_class).toBeTruthy()
    expect(JSON.stringify(attempts[0])).not.toContain("boom")
    expect(kindsOf(events, "llm_completed")).toHaveLength(0)
  })

  it("a budget-rejected request lands a zero-rung attempt", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: oversizedPromptProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
    })

    for await (const _event of runner.run({ sessionId: "evidence-rejected", goal: "too big" })) {
      // drain
    }

    const events = await eventsOf(sessionLog, "evidence-rejected")
    const attempts = kindsOf(events, "provider_attempt")
    expect(attempts.length).toBeGreaterThanOrEqual(1)
    const rejected = attempts[0]
    expect(rejected.status).toBe("rejected")
    expect(rejected.transport_rungs).toBe(0)
    expect(rejected.last_error_class).toBe("context_overflow")
    // G2: the fingerprint still binds the would-be request to its prompt_measured record.
    const measured = kindsOf(events, "prompt_measured")
    expect(measured.length).toBeGreaterThan(0)
    expect(rejected.request_fingerprint).toBe(measured[0].measurement.requestFingerprint)
  })

  it("a host interruption lands an aborted attempt", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: longStreamProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })

    for await (const event of runner.run({ sessionId: "evidence-aborted", goal: "cancel me" })) {
      if (event.type === "text_delta") runner.interrupt("user")
    }

    const events = await eventsOf(sessionLog, "evidence-aborted")
    const attempts = kindsOf(events, "provider_attempt")
    expect(attempts).toHaveLength(1)
    expect(attempts[0].status).toBe("aborted")
    expect(attempts[0].effect_id).toBeTruthy()
    expect(attempts[0].request_fingerprint).toBeTruthy()
  })

  it("attempt-record helpers pin the policy and degrade invalid measurements", () => {
    const route = {
      routeId: "sha256:r", provider: "p", protocol: "anthropic-messages" as const, model: "m",
      endpoint: { id: "p.anthropic-messages", protocol: "anthropic-messages", baseURL: "" },
      adapterVersion: "0.2.64", capabilitiesRef: "anthropic-messages",
    }
    const usage = tryNormalizeProviderUsage({ inputTokens: 100, outputTokens: 40 })
    expect(usage).toBeDefined()
    const record = providerAttemptToRecord({
      effectId: "op:step:1:effect:0",
      attemptSeq: 1,
      route,
      requestFingerprint: "fp",
      status: "success",
      transportRungs: 1,
      startedAtMs: 1,
      finishedAtMs: 2,
      usage,
    }, FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY.policyId)
    expect(record.accounting_policy_id).toBe("deepstrike.full-footprint@2026-09-15")
    expect(record.usage?.uncachedInputTokens).toBe(100)
    // An invalid frame (cache subset > input) degrades to no measurement, never a run failure.
    expect(tryNormalizeProviderUsage({ inputTokens: 10, outputTokens: 5, cacheReadInputTokens: 11 })).toBeUndefined()
  })
})
