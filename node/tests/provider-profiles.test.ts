import { endpointProfiles, modelProfiles } from "../src/providers/profiles.js"

describe("provider profiles", () => {
  it("uses MiniMax Anthropic as the default current endpoint", () => {
    expect(modelProfiles["minimax/MiniMax-M2.7"].defaultEndpointId).toBe("minimax.anthropic")
    expect(endpointProfiles["minimax.anthropic"].baseURL).toBe("https://api.minimaxi.com/anthropic")
  })

  it("keeps Kimi support on the current 2.5/2.6 family", () => {
    expect(Object.keys(modelProfiles).filter(id => id.startsWith("kimi/")).sort()).toEqual([
      "kimi/kimi-k2.5",
      "kimi/kimi-k2.6",
    ])
  })

  it("models current DeepSeek V4 endpoints and thinking controls", () => {
    expect(modelProfiles["deepseek/deepseek-v4-flash"]).toMatchObject({
      defaultEndpointId: "deepseek.openai",
      reasoning: { supported: true, preserveAcrossToolTurns: true },
    })
    expect(modelProfiles["deepseek/deepseek-v4-pro"]).toBeDefined()
  })

  it("keeps OpenAI chat and future responses endpoints distinct", () => {
    expect(endpointProfiles["openai.chat"]).toMatchObject({
      providerId: "openai",
      protocol: "openai-chat",
    })
    expect(endpointProfiles["openai.responses"]).toMatchObject({
      providerId: "openai",
      protocol: "openai-responses",
    })
  })

  it("routes OpenAI legacy chat models and GPT-5 models through distinct endpoints", () => {
    expect(modelProfiles["openai/gpt-4o"]).toMatchObject({
      providerId: "openai",
      defaultEndpointId: "openai.chat",
      reasoning: { supported: false, preserveAcrossToolTurns: false },
    })
    expect(modelProfiles["openai/gpt-5-mini"]).toMatchObject({
      providerId: "openai",
      defaultEndpointId: "openai.responses",
      reasoning: { supported: true, preserveAcrossToolTurns: true },
    })
  })
})
