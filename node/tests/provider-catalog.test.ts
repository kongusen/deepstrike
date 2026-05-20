import { createProvider } from "../src/providers/catalog.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { KimiProvider } from "../src/providers/kimi.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { MiniMaxProvider } from "../src/providers/minimax.js"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { QwenProvider } from "../src/providers/qwen.js"
import { GeminiProvider } from "../src/providers/gemini.js"
import { GLMProvider } from "../src/providers/glm.js"

describe("provider catalog", () => {
  it("creates OpenAI Chat providers from chat model profiles", () => {
    const provider = createProvider({
      model: "openai/gpt-4o",
      apiKey: "test-key",
    })

    expect(provider).toBeInstanceOf(OpenAIChatProvider)
    expect(provider).not.toBeInstanceOf(OpenAIResponsesProvider)
    expect(provider as unknown as { model: string }).toMatchObject({ model: "gpt-4o" })
  })

  it("creates OpenAI Responses providers from responses model profiles", () => {
    const provider = createProvider({
      model: "openai/gpt-5-mini",
      apiKey: "test-key",
    })

    expect(provider).toBeInstanceOf(OpenAIResponsesProvider)
    expect(provider as unknown as { model: string }).toMatchObject({ model: "gpt-5-mini" })
  })

  it("allows explicit OpenAI endpoint overrides when protocols are intentional", () => {
    const provider = createProvider({
      model: "openai/gpt-5-mini",
      endpoint: "openai.chat",
      apiKey: "test-key",
    })

    expect(provider).toBeInstanceOf(OpenAIChatProvider)
    expect(provider as unknown as { model: string }).toMatchObject({ model: "gpt-5-mini" })
  })

  it("rejects endpoint profiles from a different provider family", () => {
    expect(() => createProvider({
      model: "openai/gpt-5-mini",
      endpoint: "minimax.anthropic",
      apiKey: "test-key",
    })).toThrow("does not belong to provider openai")
  })

  it("creates profile-backed providers for the existing native catalog families", () => {
    expect(createProvider({
      model: "minimax/MiniMax-M2.7",
      apiKey: "test-key",
    })).toBeInstanceOf(MiniMaxProvider)
    expect(createProvider({
      model: "deepseek/deepseek-v4-flash",
      apiKey: "test-key",
    })).toBeInstanceOf(DeepSeekProvider)
    expect(createProvider({
      model: "kimi/kimi-k2.6",
      apiKey: "test-key",
    })).toBeInstanceOf(KimiProvider)
  })

  it("allows custom provider-prefixed model names for forward compatibility", () => {
    const provider = createProvider({
      model: "openai/gpt-next-custom",
      apiKey: "test-key",
      baseURL: "https://gateway.example.com/v1",
    })

    expect(provider).toBeInstanceOf(OpenAIChatProvider)
    expect(provider as unknown as { model: string }).toMatchObject({ model: "gpt-next-custom" })
    expect((provider as unknown as { client: { baseURL: string } }).client.baseURL).toBe("https://gateway.example.com/v1")
  })

  it("allows custom raw model names when provider or endpoint identifies the family", () => {
    expect(createProvider({
      provider: "anthropic",
      model: "claude-future",
      apiKey: "test-key",
      baseURL: "https://anthropic-gateway.example.com",
    })).toBeInstanceOf(AnthropicProvider)

    expect(createProvider({
      provider: "qwen",
      model: "qwen-future",
      apiKey: "test-key",
      baseURL: "https://dashscope-gateway.example.com/v1",
    })).toBeInstanceOf(QwenProvider)

    expect(createProvider({
      provider: "gemini",
      model: "gemini-future",
      apiKey: "test-key",
      baseURL: "https://gemini-gateway.example.com",
    })).toBeInstanceOf(GeminiProvider)

    expect(createProvider({
      endpoint: "glm.openai",
      model: "glm-future",
      apiKey: "test-key",
      baseURL: "https://glm-gateway.example.com/v4",
    })).toBeInstanceOf(GLMProvider)
  })

  it("requires provider context for custom model names without a provider prefix", () => {
    expect(() => createProvider({
      model: "future-model",
      apiKey: "test-key",
    })).toThrow("Pass provider or endpoint")
  })
})
