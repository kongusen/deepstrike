import {
  createProviderRequestPlan,
  measurementForPlan,
  normalizeProviderUsage,
  priceProviderUsage,
  recordPromptMeasurement,
} from "../src/providers/request-plan.js"
import type { RenderedContext, ToolSchema } from "../src/types.js"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const context: RenderedContext = {
  systemText: "Be precise.",
  turns: [{ role: "user", content: "Explain CJK: 你好", toolCalls: [] }],
}

const tools: ToolSchema[] = [{
  name: "lookup",
  description: "Look up a fact.",
  parameters: '{"type":"object","properties":{"q":{"type":"string"}}}',
}]

describe("spc_016-01: provider request plans", () => {
  it("uses the shared cross-SDK SHA-256 fingerprint fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/provider-request-plan/v1.json"), "utf8")) as {
      input: { providerId: string; modelId: string; endpoint: { id: string; protocol: string; baseURL: string }; context: RenderedContext; tools: ToolSchema[]; options: Record<string, unknown> }
      fingerprint: string
    }
    const plan = createProviderRequestPlan(fixture.input)
    expect(plan.fingerprint).toBe(fixture.fingerprint)
    expect(plan.options).toEqual({ temperature: 0.2, auth: { mode: "request" }, transport: {} })
  })

  it("fingerprints every material request input but excludes credentials and retry transport state", () => {
    const base = createProviderRequestPlan({
      providerId: "openai",
      modelId: "gpt-4o",
      endpoint: { id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" },
      context,
      tools,
      options: { temperature: 0.2, retry: { maxRetries: 3 }, apiKey: "secret" },
    })
    const retryOnly = createProviderRequestPlan({
      providerId: "openai",
      modelId: "gpt-4o",
      endpoint: { id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" },
      context,
      tools,
      options: { temperature: 0.2, retry: { maxRetries: 99 }, apiKey: "other-secret" },
    })
    const changedTool = createProviderRequestPlan({
      providerId: "openai",
      modelId: "gpt-4o",
      endpoint: { id: "openai.chat", protocol: "openai-chat", baseURL: "https://api.openai.com/v1" },
      context,
      tools: [{ ...tools[0], description: "Changed" }],
      options: { temperature: 0.2 },
    })

    expect(base.fingerprint).toBe(retryOnly.fingerprint)
    expect(base.fingerprint).not.toBe(changedTool.fingerprint)
    expect(JSON.stringify(base)).not.toContain("secret")
    expect(base.options).toEqual({ temperature: 0.2 })
  })

  it("normalizes full input footprint, cache splits, reasoning, and prices only a valid snapshot", () => {
    const usage = normalizeProviderUsage({
      inputTokens: 120,
      outputTokens: 30,
      cacheReadInputTokens: 20,
      cacheCreationInputTokens: 10,
      reasoningTokens: 6,
    })
    expect(usage).toEqual({
      inputTokens: 120,
      uncachedInputTokens: 90,
      outputTokens: 30,
      cacheReadInputTokens: 20,
      cacheCreationInputTokens: 10,
      reasoningTokens: 6,
    })
    expect(priceProviderUsage(usage, {
      version: "2026-08-13",
      currency: "USD",
      region: "global",
      effectiveFrom: "2026-08-01T00:00:00Z",
      expiresAt: "2026-09-01T00:00:00Z",
      ratesPerMillion: { input: 2, output: 8, cacheRead: 0.2, cacheCreation: 2.5 },
    }, "2026-08-13T00:00:00Z")).toEqual({
      currency: "USD",
      amount: 0.000449,
      pricingVersion: "2026-08-13",
      source: "snapshot",
    })
    expect(priceProviderUsage(usage, {
      version: "expired",
      currency: "USD",
      region: "global",
      effectiveFrom: "2026-01-01T00:00:00Z",
      expiresAt: "2026-02-01T00:00:00Z",
      ratesPerMillion: { input: 2, output: 8 },
    }, "2026-08-13T00:00:00Z")).toEqual({ source: "unpriced", reason: "pricing_snapshot_expired" })
  })

  it("reuses a durable measurement only for the exact same request fingerprint", () => {
    const plan = createProviderRequestPlan({
      providerId: "anthropic", modelId: "claude", endpoint: { id: "anthropic.messages", protocol: "anthropic-messages", baseURL: "https://api.anthropic.com" }, context, tools,
    })
    const measurement = recordPromptMeasurement(plan, {
      inputTokens: 42,
      source: { kind: "native", provider: "anthropic" },
      confidence: "exact",
    })
    const changed = createProviderRequestPlan({
      providerId: "anthropic", modelId: "claude", endpoint: { id: "anthropic.messages", protocol: "anthropic-messages", baseURL: "https://api.anthropic.com" }, context: { ...context, systemText: "Changed" }, tools,
    })

    expect(measurementForPlan(plan, measurement)).toEqual(measurement)
    expect(measurementForPlan(changed, measurement)).toBeUndefined()
  })
})
