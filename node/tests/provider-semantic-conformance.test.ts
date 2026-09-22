import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createProviderRequestPlan, measurementForPlan, recordPromptMeasurement, resolveProviderRoute } from "../src/providers/request-plan.js"
import { providerAttemptToRecord, FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY } from "../src/runtime/execution-evidence.js"
import type { RenderedContext } from "../src/types.js"
import { AnthropicMessagesAdapter } from "../src/providers/anthropic-adapter.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import { OpenAIResponsesAdapter } from "../src/providers/openai-responses-adapter.js"
import { GeminiAdapter } from "../src/providers/gemini-adapter.js"
import { OllamaAdapter } from "../src/providers/ollama-adapter.js"

const context: RenderedContext = { systemText: "", turns: [{ role: "user", content: "hello" }] }

test("SPC-028-11/13 route identity changes with execution target", () => {
  const a = resolveProviderRoute({ descriptor: () => ({ provider: "openai", protocol: "openai-chat", model: "gpt-a" }) })
  const b = resolveProviderRoute({ descriptor: () => ({ provider: "openai", protocol: "openai-chat", model: "gpt-b" }) })
  expect(a.routeId).not.toBe(b.routeId)
  expect(a.provider).toBe("openai")
  expect(a.protocol).toBe("openai-chat")
})

test("SPC-028-19/20 measurements are reusable only for exact request identity", () => {
  const plan = createProviderRequestPlan({ providerId: "p", modelId: "m", endpoint: { id: "e", protocol: "openai-chat", baseURL: "" }, context, tools: [] })
  const changed = createProviderRequestPlan({ providerId: "p", modelId: "m", endpoint: { id: "e", protocol: "openai-chat", baseURL: "" }, context: { ...context, turns: [{ role: "user", content: "changed" }] }, tools: [] })
  const measurement = recordPromptMeasurement(plan, { inputTokens: 3, source: { kind: "local_exact", tokenizer: "test" }, confidence: "exact" })
  expect(measurementForPlan(plan, measurement)).toEqual(measurement)
  expect(measurementForPlan(changed, measurement)).toBeUndefined()
})

test("SPC-028-23/25/26 settlement and attempt evidence preserve policy identity", () => {
  const record = providerAttemptToRecord({
    effectId: "effect-1", attemptSeq: 1, route: resolveProviderRoute({ descriptor: () => ({ provider: "p", protocol: "openai-chat", model: "m" }) }), requestFingerprint: "fp", status: "success", transportRungs: 1, startedAtMs: 1, finishedAtMs: 2,
  }, FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY.policyId)
  expect(record.accounting_policy_id).toBe(FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY.policyId)
  expect(record.request_fingerprint).toBe("fp")
})

test("SPC-028-58/60 shared provider conformance fixture lists every supported protocol", () => {
  const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/runtime-language/provider-conformance.json"), "utf8")) as { protocols: Record<string, string[]>; measurementSources: string[] }
  expect(Object.keys(fixture.protocols)).toEqual(expect.arrayContaining(["anthropic-messages", "openai-chat", "openai-responses", "gemini", "ollama-chat"]))
  expect(fixture.measurementSources).toEqual(expect.arrayContaining(["native", "local_exact", "heuristic", "postflight", "unavailable"]))
})

test.each([
  ["anthropic-messages", () => new AnthropicMessagesAdapter()],
  ["openai-chat", () => new OpenAIChatAdapter()],
  ["openai-responses", () => new OpenAIResponsesAdapter()],
  ["gemini", () => new GeminiAdapter()],
  ["ollama-chat", () => new OllamaAdapter()],
])("SPC-028-58 adapter implements the declared protocol contract: %s", (protocol, create) => {
  const adapter = create()
  expect(adapter.protocol).toBe(protocol)
  expect(adapter.protocolCapabilities).toBeDefined()
  for (const method of ["buildRequest", "decodeComplete", "createStreamState", "pushStreamChunk", "finishStream", "normalizeUsage", "normalizeStopReason"]) {
    expect(typeof adapter[method as keyof typeof adapter]).toBe("function")
  }
})
