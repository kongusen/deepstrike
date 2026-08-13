import * as providers from "../src/providers/public.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { GeminiProvider } from "../src/providers/gemini.js"
import { OllamaProvider } from "../src/providers/ollama.js"
import { createProvider } from "../src/providers/catalog.js"
import {
  normalizeAnthropicUsage,
  normalizeGeminiUsage,
  normalizeOllamaUsage,
  normalizeOpenAIUsage,
} from "../src/providers/usage-normalizer.js"

describe("spc_013-A-00R corrective cleanup", () => {
  it("keeps unpublished mixed capability accessors out of the providers public subpath", () => {
    expect(providers).not.toHaveProperty("getModelCapabilities")
    expect(providers).not.toHaveProperty("modelProfiles")
    expect(providers).not.toHaveProperty("getModelProfile")
  })

  it("does not expose the unused ProviderProfile view on any runtime provider", () => {
    const instances: object[] = [
      new AnthropicProvider({ apiKey: "k", model: "claude-opus-4-1" }),
      new OpenAIChatProvider({ apiKey: "k", model: "gpt-4o" }),
      new OpenAIResponsesProvider("k", "gpt-4.1"),
      new GeminiProvider("k", "gemini-2.0-flash"),
      new OllamaProvider("llama3"),
      createProvider({ model: "deepseek/deepseek-chat", apiKey: "k" }),
    ]
    for (const provider of instances) expect(provider).not.toHaveProperty("profile")
  })

  it("does not fabricate zero usage when the provider supplied no usage fields", () => {
    expect(normalizeOpenAIUsage(undefined)).toBeUndefined()
    expect(normalizeAnthropicUsage({})).toBeUndefined()
    expect(normalizeGeminiUsage(null)).toBeUndefined()
    expect(normalizeOllamaUsage({ done: true })).toBeUndefined()
  })
})
