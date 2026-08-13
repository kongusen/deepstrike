import { createProviderRequestPlan, createProviderRequestPlanForProvider, estimateProviderPromptTokens, measurementForPlan, normalizeProviderUsage, priceProviderUsage, recordPromptMeasurement } from "../src/providers/request-plan.js"
import { contentDispositionFor, requireContentDisposition } from "../src/providers/content-policy.js"
import { contentDispositionFor as exportedContentDispositionFor } from "../src/index.js"
import { decodeCanonicalStopReason, normalizeProviderStopReason } from "../src/providers/stop-reason.js"
import { OpenAIProvider, QwenProvider } from "../src/providers/openai.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { readFileSync } from "node:fs"
import { join } from "node:path"

describe("spc_016-01 WASM request plan", () => {
  it("uses the shared cross-SDK SHA-256 fingerprint fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/provider-request-plan/v1.json"), "utf8")) as {
      input: Parameters<typeof createProviderRequestPlan>[0]
      fingerprint: string
    }
    const plan = createProviderRequestPlan(fixture.input)
    expect(plan.fingerprint).toBe(fixture.fingerprint)
    expect(plan.options).toEqual({ temperature: 0.2, auth: { mode: "request" }, transport: {} })
  })

  it("binds provider plans to resolved endpoint identity", () => {
    const context = { systemText: "system", turns: [{ role: "user" as const, content: "hello" }] }
    const provider = {
      descriptor: () => ({ provider: "openai", protocol: "openai-chat" as const, model: "gpt-4o" }),
      requestPlanIdentity: () => ({
        providerId: "tenant-openai",
        modelId: "wire-gpt-4o",
        endpoint: { id: "tenant.chat", protocol: "openai-chat" as const, baseURL: "https://tenant.example/v1" },
      }),
    }
    const plan = createProviderRequestPlanForProvider(provider, context, [])
    expect(plan.providerId).toBe("tenant-openai")
    expect(plan.modelId).toBe("wire-gpt-4o")
    expect(plan.endpoint).toEqual({ id: "tenant.chat", protocol: "openai-chat", baseURL: "https://tenant.example/v1" })
    expect(plan.fingerprint).not.toBe(createProviderRequestPlan({
      providerId: "tenant-openai",
      modelId: "wire-gpt-4o",
      endpoint: { id: "tenant.chat", protocol: "openai-chat", baseURL: "https://other.example/v1" },
      context,
      tools: [],
    }).fingerprint)
  })

  it("built-in providers expose the endpoint used by their wire request", () => {
    expect(createProviderRequestPlanForProvider(new OpenAIProvider("secret"), contextForPlan(), []).endpoint)
      .toEqual({ id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" })
    expect(createProviderRequestPlanForProvider(new QwenProvider("secret"), contextForPlan(), []).endpoint)
      .toEqual({ id: "qwen.dashscope", protocol: "openai-chat", baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1" })
    expect(createProviderRequestPlanForProvider(new AnthropicProvider("secret"), contextForPlan(), []).endpoint)
      .toEqual({ id: "anthropic.messages", protocol: "anthropic-messages", baseURL: "https://api.anthropic.com" })
  })

  it("uses canonical serialization for heuristic prompt estimates", () => {
    const context = { systemText: "system", turns: [{ role: "user" as const, content: "hello" }] }
    const first = estimateProviderPromptTokens(context, [{ name: "a", description: "A", parameters: "{}" }])
    const second = estimateProviderPromptTokens(context, [{ parameters: "{}", description: "A", name: "a" }])
    expect(second).toBe(first)
  })

  it("normalizes usage and prices only a valid time-bound snapshot", () => {
    const usage = normalizeProviderUsage({ inputTokens: 120, outputTokens: 30, cacheReadInputTokens: 20, cacheCreationInputTokens: 10, reasoningTokens: 6 })
    expect(usage).toEqual({ inputTokens: 120, uncachedInputTokens: 90, outputTokens: 30, cacheReadInputTokens: 20, cacheCreationInputTokens: 10, reasoningTokens: 6 })
    expect(priceProviderUsage(usage, {
      version: "2026-08-13", currency: "USD", region: "global", effectiveFrom: "2026-08-01T00:00:00Z", expiresAt: "2026-09-01T00:00:00Z",
      ratesPerMillion: { input: 2, output: 8, cacheRead: 0.2, cacheCreation: 2.5 },
    }, "2026-08-13T00:00:00Z")).toEqual({ source: "snapshot", currency: "USD", amount: 0.000449, pricingVersion: "2026-08-13" })
    expect(priceProviderUsage(usage, {
      version: "expired", currency: "USD", region: "global", effectiveFrom: "2026-01-01T00:00:00Z", expiresAt: "2026-02-01T00:00:00Z",
      ratesPerMillion: { input: 2, output: 8 },
    }, "2026-08-13T00:00:00Z")).toEqual({ source: "unpriced", reason: "pricing_snapshot_expired" })
  })

  it("excludes secret and retry-only options while binding measurements to the exact fingerprint", () => {
    const args = { providerId: "openai", modelId: "gpt", endpoint: { id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" }, context: { systemText: "s", turns: [{ role: "user" as const, content: "你好" }] }, tools: [] }
    const first = createProviderRequestPlan({ ...args, options: { apiKey: "secret", headers: { Authorization: "Bearer secret" }, accessToken: "secret", retry: { max: 1 }, temperature: 0.2 } })
    const same = createProviderRequestPlan({ ...args, options: { apiKey: "other", headers: { Authorization: "Bearer other" }, accessToken: "other", retry: { max: 99 }, temperature: 0.2 } })
    const changed = createProviderRequestPlan({ ...args, modelId: "gpt-next", options: { temperature: 0.2 } })
    const measurement = recordPromptMeasurement(first, { inputTokens: 14, source: { kind: "heuristic" }, confidence: "low_confidence" })
    expect(first.fingerprint).toBe(same.fingerprint)
    expect(first.fingerprint).not.toBe(changed.fingerprint)
    expect(JSON.stringify(first)).not.toContain("secret")
    expect(measurementForPlan(first, measurement)).toEqual(measurement)
    expect(measurementForPlan(changed, measurement)).toBeUndefined()
    expect(measurementForPlan(first, { ...measurement, inputTokens: -1 })).toBeUndefined()
  })

  it("rejects malformed pricing snapshots", () => {
    const usage = normalizeProviderUsage({ inputTokens: 1, outputTokens: 1 })
    expect(priceProviderUsage(usage, { version: "bad", currency: "USD", region: "global", effectiveFrom: "2026-01-01T00:00:00Z", ratesPerMillion: { input: Number.NaN, output: 1 } }, "2026-01-02T00:00:00Z")).toEqual({ source: "unpriced", reason: "invalid_pricing_snapshot" })
  })
})

function contextForPlan() {
  return { systemText: "system", turns: [{ role: "user" as const, content: "hello" }] }
}

describe("spc_015-06 WASM content policy", () => {
  it("keeps bridge outcomes explicit and fails closed for unsupported protocol shapes", () => {
    expect(exportedContentDispositionFor("openai-chat", "file", "tool_result")).toBe("bridge")
    expect(contentDispositionFor("openai-chat", "file", "tool_result")).toBe("bridge")
    expect(contentDispositionFor("openai-responses", "file", "message")).toBe("native")
    expect(() => requireContentDisposition("anthropic-messages", "video", "message"))
      .toThrow("Unsupported content policy: video")
  })

  it("matches the shared cross-SDK content policy fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/provider-content-policy/v1.json"), "utf8")) as {
      cases: Array<{ protocol: string; modality: "text" | "image" | "audio" | "video" | "file"; placement: "message" | "tool_result"; disposition: string }>
    }
    for (const testCase of fixture.cases) {
      expect(contentDispositionFor(testCase.protocol, testCase.modality, testCase.placement)).toBe(testCase.disposition)
    }
  })
})

describe("WASM provider stop reasons", () => {
  it("maps a provider-specific stop reason to other without relaxing the fixture decoder", () => {
    expect(normalizeProviderStopReason("length")).toBe("other")
    expect(() => decodeCanonicalStopReason("length")).toThrow("unknown canonical stop reason")
    expect(() => normalizeProviderStopReason(""))
      .toThrow("invalid provider stop reason")
  })
})
