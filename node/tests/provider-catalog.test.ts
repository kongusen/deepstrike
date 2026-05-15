import { createProvider } from "../src/providers/catalog.js"
import { DeepSeekProvider, KimiProvider, OpenAIChatProvider } from "../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { MiniMaxProvider } from "../src/providers/minimax.js"

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
})
