import { PROVIDER_REGISTRY, providerRegistryKey } from "../src/providers/registry.js"
import { endpointProfiles } from "../src/providers/profiles.js"

// Endpoint protocols that produce a chat/completions provider (vs embeddings).
const CHAT_PROTOCOLS = new Set(["anthropic-messages", "openai-chat", "openai-responses", "gemini"])

describe("provider registry (P3)", () => {
  it("contains exactly the expected (vendor, wire) pairs", () => {
    expect(new Set(Object.keys(PROVIDER_REGISTRY))).toEqual(new Set([
      "anthropic:anthropic-messages",
      "openai:openai-chat",
      "openai:openai-responses",
      "deepseek:openai-chat",
      "deepseek:anthropic-messages",
      "kimi:openai-chat",
      "kimi:anthropic-messages",
      "qwen:openai-chat",
      "qwen:anthropic-messages",
      "glm:openai-chat",
      "glm:anthropic-messages",
      "minimax:openai-chat",
      "minimax:anthropic-messages",
      "gemini:gemini",
    ]))
  })

  it("routes every chat endpoint profile to a registry maker (no unroutable endpoint)", () => {
    for (const endpoint of Object.values(endpointProfiles)) {
      if (!CHAT_PROTOCOLS.has(endpoint.protocol)) continue
      const key = providerRegistryKey(endpoint.providerId, endpoint.protocol)
      expect(PROVIDER_REGISTRY[key]).toBeDefined()
    }
  })

  it("makers construct a provider with the requested model", () => {
    const p = PROVIDER_REGISTRY["deepseek:openai-chat"]("k", "deepseek-chat", undefined, undefined)
    expect((p as unknown as { model: string }).model).toBe("deepseek-chat")
  })
})
