import { createProvider, resolveProviderRuntime } from "../src/providers/catalog.js"
import { tokenMeasurementEvidence } from "../src/providers/model-registry.js"

describe("SPC-024 executable token measurement capabilities", () => {
  it("records official API and adapter evidence separately", () => {
    for (const entry of tokenMeasurementEvidence) {
      expect(entry.source).toMatch(/^https:\/\//)
      expect(entry.verifiedAt).toBe("2026-08-26")
      expect(["supported", "unsupported", "unknown"]).toContain(entry.providerApiState)
      expect(["available", "unavailable"]).toContain(entry.adapterState)
      expect(["provider_preflight", "official_local_tokenizer", "postflight", "heuristic"])
        .toContain(entry.method)
      expect(entry.coverage.length).toBeGreaterThan(0)
      expect(entry.sdk).toBeTruthy()
    }
  })

  it.each([
    ["anthropic/claude-sonnet-4-6", "anthropic.messages"],
    ["gemini/gemini-2.5-pro", "gemini.google"],
    ["openai/gpt-5.2", "openai.responses"],
  ] as const)("only reports runtime support when %s exposes countTokens", (model, endpoint) => {
    const runtime = resolveProviderRuntime({ model, apiKey: "test", endpoint })
    const provider = createProvider({ model, apiKey: "test", endpoint })

    if (runtime.effectiveCapabilities.nativeTokenCounting.state === "supported") {
      expect(typeof provider.countTokens).toBe("function")
    }
  })

  it("does not extend Responses input counting to Chat Completions", () => {
    const chat = resolveProviderRuntime({ model: "openai/gpt-4o", apiKey: "test" })
    expect(chat.effectiveCapabilities.nativeTokenCounting.state).not.toBe("supported")
    const chatProvider = createProvider({ model: "openai/gpt-4o", apiKey: "test" })
    expect(typeof chatProvider.countTokens).toBe("undefined")
  })

  it("keeps Responses counting behind endpoint identity like Anthropic", () => {
    const unverified = resolveProviderRuntime({
      model: "openai/gpt-5.2", apiKey: "test", baseURL: "https://proxy.invalid/v1",
    })
    expect(unverified.effectiveCapabilities.nativeTokenCounting.state).toBe("unknown")

    const explicit = resolveProviderRuntime({
      model: "openai/gpt-5.2", apiKey: "test", endpoint: "openai.responses", baseURL: "https://proxy.invalid/v1",
    })
    expect(explicit.effectiveCapabilities.nativeTokenCounting.state).toBe("supported")
  })
})
