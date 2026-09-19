import { createContextPreparationAdapter } from "../src/runtime/context.js"
import { canonicalActionFromProjectionJson } from "../src/runtime/canonical-kernel-step.js"
import type { ContextProviderPreparationRequest } from "../src/runtime/context.js"

const effect = { context: { system_text: "policy", turns: [], tools: [] }, tools: [], context_candidate: { marker: "kernel-owned" } }
const request = {
  effect,
  request_fingerprint: "request-1",
  provider_route: { routeId: "route-1" },
  prompt_measurement: { requestFingerprint: "request-1", inputTokens: 2, source: { kind: "heuristic" }, confidence: "low_confidence" },
} as unknown as ContextProviderPreparationRequest

test("preserves the canonical effect payload before provider projection", () => {
  const action = canonicalActionFromProjectionJson(JSON.stringify({ state: "pending", action: { kind: "call_provider", effect_id: "effect-1", payload: effect } }))
  expect(action?.kind).toBe("call_provider")
  if (action?.kind !== "call_provider") throw new Error("expected provider action")
  expect(action.contextEffect).toEqual(effect)
  expect(action.contextEffect).not.toHaveProperty("kind")
})

test("delegates frozen effect and host evidence unchanged to the core", () => {
  const result = { execution_input: { input_digest: "core-owned" }, binding: { digest: "binding" }, plan: {} }
  const adapter = createContextPreparationAdapter(raw => {
    expect(JSON.parse(raw)).toEqual(request)
    return JSON.stringify(result)
  }, () => "true")
  expect(adapter.prepare(request)).toEqual(result)
})

test("propagates canonical validation rejection without fallback", () => {
  const adapter = createContextPreparationAdapter(() => { throw new Error("candidate mismatch") }, () => "true")
  expect(() => adapter.prepare(request)).toThrow("candidate mismatch")
})

import { RuntimeRunner, collectText } from "../src/runtime/runner.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { LLMProvider, StreamEvent } from "../src/types.js"

test("persists a verified input before dispatching the provider", async () => {
  const log = new InMemorySessionLog()
  let dispatched = false
  const provider: LLMProvider = {
    async complete() { return { role: "assistant", content: "done", toolCalls: [] } },
    async *stream(): AsyncIterable<StreamEvent> {
      const prepared = (await log.read("context-order")).find(entry => entry.event.kind === "context_prepared")?.event
      expect(prepared?.kind).toBe("context_prepared")
      if (prepared?.kind !== "context_prepared") throw new Error("input not persisted")
      expect(prepared.preparation.execution_input.input_digest).toMatch(/^sha256:/)
      expect(prepared.preparation.binding.execution_input).toBe(prepared.preparation.execution_input.input_digest)
      expect(prepared.preparation.plan.state_digest).toBe(prepared.preparation.state.digest)
      dispatched = true
      yield { type: "text_delta", delta: "done" }
    },
  }
  const runner = new RuntimeRunner({ provider, sessionLog: log, executionPlane: new LocalExecutionPlane(), maxTokens: 8_000, maxTurns: 2 })
  expect(await collectText(runner.run({ sessionId: "context-order", goal: "finish" }))).toBe("done")
  expect(dispatched).toBe(true)
})


test("delegates recorded preparation verification and rejects altered evidence", () => {
  const prepared = { execution_input: { input_digest: "recorded" } } as unknown as import("../src/runtime/context.js").ContextPrepared
  const adapter = createContextPreparationAdapter(() => "{}", raw => {
    const verification = JSON.parse(raw)
    expect(verification.effect).toEqual(effect)
    if (verification.preparation.execution_input.input_digest !== "recorded") throw new Error("preparation mismatch")
    return "true"
  })
  expect(adapter.verify(effect, prepared)).toBe(true)
  expect(() => adapter.verify(effect, { ...prepared, execution_input: { ...prepared.execution_input, input_digest: "altered" } })).toThrow("preparation mismatch")
})
