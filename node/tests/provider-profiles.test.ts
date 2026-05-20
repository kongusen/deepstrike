import { endpointProfiles, modelProfiles } from "../src/providers/profiles.js"

describe("provider profiles", () => {
  it("uses MiniMax Anthropic as the default current endpoint", () => {
    expect(modelProfiles["minimax/MiniMax-M2.7"].defaultEndpointId).toBe("minimax.anthropic")
    expect(endpointProfiles["minimax.anthropic"].baseURL).toBe("https://api.minimaxi.com/anthropic")
  })

  it("registers Kimi legacy moonshot-v1 and current k2 profiles", () => {
    expect(Object.keys(modelProfiles).filter(id => id.startsWith("kimi/")).sort()).toEqual([
      "kimi/kimi-k2-thinking",
      "kimi/kimi-k2-thinking-turbo",
      "kimi/kimi-k2.5",
      "kimi/kimi-k2.6",
      "kimi/moonshot-v1-128k",
      "kimi/moonshot-v1-32k",
      "kimi/moonshot-v1-8k",
    ])
    expect(modelProfiles["kimi/kimi-k2.6"]).toMatchObject({
      providerId: "kimi",
      defaultEndpointId: "kimi.openai",
      reasoning: { supported: true },
    })
  })

  it("registers latest OpenAI, MiniMax, Qwen, and Gemini chat model families", () => {
    expect(modelProfiles["anthropic/claude-opus-4-1"]).toMatchObject({
      defaultEndpointId: "anthropic.messages",
      contextWindow: 200_000,
      reasoning: { supported: true, preserveAcrossToolTurns: true },
    })
    expect(modelProfiles["openai/gpt-5.5"]).toMatchObject({
      defaultEndpointId: "openai.responses",
      contextWindow: 1_000_000,
      reasoning: { supported: true, preserveAcrossToolTurns: true },
      policy: { maxTurns: 60 },
    })
    expect(modelProfiles["openai/gpt-5.4-nano"]).toMatchObject({
      defaultEndpointId: "openai.responses",
      contextWindow: 400_000,
    })
    expect(modelProfiles["minimax/MiniMax-M2.7-highspeed"]).toMatchObject({
      defaultEndpointId: "minimax.anthropic",
      contextWindow: 204_800,
    })
    expect(modelProfiles["qwen/qwen3.7-max-preview"]).toMatchObject({
      defaultEndpointId: "qwen.dashscope",
      contextWindow: 256_000,
      reasoning: { supported: true },
    })
    expect(modelProfiles["qwen/qwen3.6-27b"]).toMatchObject({
      defaultEndpointId: "qwen.dashscope",
      contextWindow: 256_000,
      reasoning: { supported: true },
    })
    expect(modelProfiles["qwen/qwen3.5-plus"]).toMatchObject({
      defaultEndpointId: "qwen.dashscope",
      contextWindow: 1_000_000,
      reasoning: { supported: true },
    })
    expect(modelProfiles["qwen/text-embedding-v4"]).toMatchObject({
      defaultEndpointId: "qwen.dashscope.embeddings",
      modalities: { input: ["text"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(modelProfiles["qwen/qwen3-vl-embedding"]).toMatchObject({
      defaultEndpointId: "qwen.dashscope.multimodal-embeddings",
      modalities: { input: ["text", "image", "video"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(modelProfiles["openai/text-embedding-3-large"]).toMatchObject({
      defaultEndpointId: "openai.embeddings",
      modalities: { input: ["text"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(modelProfiles["gemini/gemini-embedding-001"]).toMatchObject({
      defaultEndpointId: "gemini.google.embeddings",
      modalities: { input: ["text"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(modelProfiles["gemini/gemini-embedding-2"]).toMatchObject({
      defaultEndpointId: "gemini.google.embeddings",
      contextWindow: 8_192,
      modalities: { input: ["text", "image", "audio", "video", "pdf"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(Object.entries(modelProfiles).filter(([id, profile]) => (
      id.startsWith("qwen/qwen3-") && profile.modalities.output.includes("text")
    ))).toEqual([])
    expect(modelProfiles["gemini/gemini-3-pro-preview"]).toMatchObject({
      defaultEndpointId: "gemini.google",
      contextWindow: 1_048_576,
      reasoning: { supported: true },
    })
    expect(modelProfiles["gemini/gemini-3.5-flash"]).toMatchObject({
      defaultEndpointId: "gemini.google",
      reasoning: { supported: true },
      policy: { maxTurns: 30 },
    })
    expect(endpointProfiles["openai.embeddings"].protocol).toBe("openai-embeddings")
    expect(endpointProfiles["qwen.dashscope.embeddings"].protocol).toBe("openai-embeddings")
    expect(endpointProfiles["qwen.dashscope.multimodal-embeddings"].protocol).toBe("dashscope-multimodal-embeddings")
    expect(endpointProfiles["gemini.google.embeddings"].protocol).toBe("gemini-embeddings")
  })

  it("models current DeepSeek V4 endpoints and thinking controls", () => {
    expect(modelProfiles["deepseek/deepseek-v4-flash"]).toMatchObject({
      defaultEndpointId: "deepseek.openai",
      reasoning: { supported: true, preserveAcrossToolTurns: true },
    })
    expect(modelProfiles["deepseek/deepseek-v4-pro"]).toBeDefined()
  })

  it("registers GLM 5.1 and GLM 4 profiles on the OpenAI-compatible endpoint", () => {
    expect(Object.keys(modelProfiles).filter(id => id.startsWith("glm/")).sort()).toEqual([
      "glm/embedding-2",
      "glm/embedding-3",
      "glm/glm-4-air",
      "glm/glm-4-flash",
      "glm/glm-4-plus",
      "glm/glm-5.1",
    ])
    expect(modelProfiles["glm/glm-5.1"]).toMatchObject({
      providerId: "glm",
      defaultEndpointId: "glm.openai",
      contextWindow: 200_000,
      reasoning: { supported: true, preserveAcrossToolTurns: true },
      policy: { maxTurns: 50 },
    })
    expect(modelProfiles["glm/embedding-3"]).toMatchObject({
      providerId: "glm",
      defaultEndpointId: "glm.openai.embeddings",
      modalities: { input: ["text"], output: ["embedding"] },
      tools: { supported: false },
    })
  })

  it("registers BAAI BGE embedding profiles as self-hosted embeddings", () => {
    expect(Object.keys(modelProfiles).filter(id => id.startsWith("baai/")).sort()).toEqual([
      "baai/bge-base-en-v1.5",
      "baai/bge-base-zh-v1.5",
      "baai/bge-code-v1",
      "baai/bge-large-en-v1.5",
      "baai/bge-large-zh-v1.5",
      "baai/bge-m3",
      "baai/bge-small-en-v1.5",
      "baai/bge-small-zh-v1.5",
      "baai/bge-vl-v1.5-mmeb",
      "baai/bge-vl-v1.5-zs",
    ])
    expect(endpointProfiles["baai.self-hosted.embeddings"].protocol).toBe("self-hosted-embeddings")
    expect(modelProfiles["baai/bge-m3"]).toMatchObject({
      providerId: "baai",
      defaultEndpointId: "baai.self-hosted.embeddings",
      contextWindow: 8_192,
      modalities: { input: ["text"], output: ["embedding"] },
      tools: { supported: false },
    })
    expect(modelProfiles["baai/bge-vl-v1.5-mmeb"]).toMatchObject({
      providerId: "baai",
      defaultEndpointId: "baai.self-hosted.embeddings",
      modalities: { input: ["text", "image"], output: ["embedding"] },
      tools: { supported: false },
    })
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
