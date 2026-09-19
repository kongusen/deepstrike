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
