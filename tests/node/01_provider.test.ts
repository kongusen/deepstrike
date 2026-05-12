/**
 * 01_provider.test.ts — OpenAIProvider streaming + circuit breaker
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { CircuitBreaker, normalizeToolCall } from "@deepstrike/sdk"
import type { StreamEvent, TextDelta } from "@deepstrike/sdk"
import { makeProvider } from "./helpers.js"

// ─── CircuitBreaker (offline) ─────────────────────────────────────────────

describe("CircuitBreaker", () => {
  it("starts closed", () => {
    const cb = new CircuitBreaker()
    assert.equal(cb.isOpen(), false)
  })

  it("opens after threshold failures", () => {
    const cb = new CircuitBreaker(3, 60_000)
    cb.recordFailure(); cb.recordFailure(); cb.recordFailure()
    assert.equal(cb.isOpen(), true)
  })

  it("resets immediately on success", () => {
    const cb = new CircuitBreaker(2, 60_000)
    cb.recordFailure(); cb.recordFailure()
    cb.recordSuccess()
    assert.equal(cb.isOpen(), false)
  })

  it("auto-resets after timeout window", async () => {
    const cb = new CircuitBreaker(2, 50)
    cb.recordFailure(); cb.recordFailure()
    assert.equal(cb.isOpen(), true)
    await new Promise(r => setTimeout(r, 60))
    assert.equal(cb.isOpen(), false)
  })
})

describe("normalizeToolCall", () => {
  it("returns null for empty name", () => {
    assert.equal(normalizeToolCall("id", "", {}), null)
  })

  it("parses JSON string arguments", () => {
    const tc = normalizeToolCall("id", "tool", '{"x":1}')
    assert.ok(tc)
    assert.deepEqual(JSON.parse(tc.arguments), { x: 1 })
  })

  it("accepts object arguments", () => {
    const tc = normalizeToolCall("id", "tool", { y: 2 })
    assert.ok(tc)
    assert.deepEqual(JSON.parse(tc.arguments), { y: 2 })
  })
})

// ─── Provider streaming (real API) ───────────────────────────────────────

describe("OpenAIProvider", () => {
  it("stream() emits text_delta events", { timeout: 60_000 }, async () => {
    const provider = makeProvider()
    const events: StreamEvent[] = []
    for await (const evt of provider.stream(
      [{ role: "user", content: "Reply with exactly: hello", toolCalls: [] }],
      [],
    )) {
      events.push(evt)
    }
    const full = events.filter(e => e.type === "text_delta").map(e => (e as TextDelta).delta).join("")
    assert.ok(full.length > 0)
    assert.ok(full.toLowerCase().includes("hello"), `got: ${full}`)
  })

  it("stream() produces tool_call events when a tool is required", { timeout: 60_000 }, async () => {
    const provider = makeProvider()
    const tools = [{
      name: "get_time",
      description: "Get the current time",
      parameters: JSON.stringify({ type: "object", properties: {}, required: [] }),
    }]
    const events: StreamEvent[] = []
    for await (const evt of provider.stream(
      [{ role: "user", content: "Call get_time right now.", toolCalls: [] }],
      tools,
    )) {
      events.push(evt)
    }
    assert.ok(events.some(e => e.type === "tool_call"), "expected tool_call event")
  })
})
