export type ProviderId = "anthropic" | "openai" | "minimax" | "deepseek" | "kimi" | "qwen" | "gemini"
export type EndpointProtocol = "anthropic-messages" | "openai-chat" | "openai-responses" | "gemini"

export interface EndpointProfile {
  id: string
  providerId: ProviderId
  protocol: EndpointProtocol
  baseURL: string
}

export interface ModelProfile {
  id: string
  providerId: ProviderId
  defaultEndpointId: string
  contextWindow?: number
  modalities: {
    input: Array<"text" | "image" | "audio">
    output: Array<"text">
  }
  tools: {
    supported: boolean
  }
  reasoning: {
    supported: boolean
    preserveAcrossToolTurns: boolean
  }
  /** Recommended runtime execution policy for this model. */
  policy?: {
    maxTurns?: number
    timeoutMs?: number
  }
}

export const endpointProfiles = {
  "anthropic.messages": {
    id: "anthropic.messages",
    providerId: "anthropic",
    protocol: "anthropic-messages",
    baseURL: "https://api.anthropic.com",
  },
  "openai.chat": {
    id: "openai.chat",
    providerId: "openai",
    protocol: "openai-chat",
    baseURL: "https://api.openai.com/v1",
  },
  "openai.responses": {
    id: "openai.responses",
    providerId: "openai",
    protocol: "openai-responses",
    baseURL: "https://api.openai.com/v1",
  },
  "minimax.anthropic": {
    id: "minimax.anthropic",
    providerId: "minimax",
    protocol: "anthropic-messages",
    baseURL: "https://api.minimaxi.com/anthropic",
  },
  "deepseek.openai": {
    id: "deepseek.openai",
    providerId: "deepseek",
    protocol: "openai-chat",
    baseURL: "https://api.deepseek.com",
  },
  "kimi.openai": {
    id: "kimi.openai",
    providerId: "kimi",
    protocol: "openai-chat",
    baseURL: "https://api.moonshot.cn/v1",
  },
  "qwen.dashscope": {
    id: "qwen.dashscope",
    providerId: "qwen",
    protocol: "openai-chat",
    baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1",
  },
  "gemini.google": {
    id: "gemini.google",
    providerId: "gemini",
    protocol: "gemini",
    baseURL: "https://generativelanguage.googleapis.com",
  },
} as const satisfies Record<string, EndpointProfile>

export const modelProfiles = {
  "anthropic/claude-opus-4-7": {
    id: "anthropic/claude-opus-4-7",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "anthropic/claude-opus-4-6": {
    id: "anthropic/claude-opus-4-6",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "anthropic/claude-sonnet-4-6": {
    id: "anthropic/claude-sonnet-4-6",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "anthropic/claude-haiku-4-5": {
    id: "anthropic/claude-haiku-4-5",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  // ── OpenAI ─────────────────────────────────────────────────────────────────
  "openai/gpt-4o": {
    id: "openai/gpt-4o", providerId: "openai", defaultEndpointId: "openai.chat",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "openai/gpt-4o-mini": {
    id: "openai/gpt-4o-mini", providerId: "openai", defaultEndpointId: "openai.chat",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  "openai/gpt-4.1": {
    id: "openai/gpt-4.1", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 1_047_576,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "openai/gpt-4.1-mini": {
    id: "openai/gpt-4.1-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 1_047_576,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "openai/gpt-4.1-nano": {
    id: "openai/gpt-4.1-nano", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 1_047_576,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  "openai/gpt-5": {
    id: "openai/gpt-5", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "openai/gpt-5-mini": {
    id: "openai/gpt-5-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "openai/o3": {
    id: "openai/o3", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "openai/o3-mini": {
    id: "openai/o3-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "openai/o4-mini": {
    id: "openai/o4-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  // ── MiniMax ────────────────────────────────────────────────────────────────
  "minimax/MiniMax-M2.7": {
    id: "minimax/MiniMax-M2.7", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 35 },
  },
  "minimax/MiniMax-M2.5": {
    id: "minimax/MiniMax-M2.5", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "minimax/MiniMax-Text-01": {
    id: "minimax/MiniMax-Text-01", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 1_000_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  // ── DeepSeek ───────────────────────────────────────────────────────────────
  "deepseek/deepseek-chat": {
    id: "deepseek/deepseek-chat", providerId: "deepseek", defaultEndpointId: "deepseek.openai",
    contextWindow: 64_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "deepseek/deepseek-reasoner": {
    id: "deepseek/deepseek-reasoner", providerId: "deepseek", defaultEndpointId: "deepseek.openai",
    contextWindow: 64_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "deepseek/deepseek-v4-flash": {
    id: "deepseek/deepseek-v4-flash", providerId: "deepseek", defaultEndpointId: "deepseek.openai",
    contextWindow: 1_000_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 20 },
  },
  "deepseek/deepseek-v4-pro": {
    id: "deepseek/deepseek-v4-pro", providerId: "deepseek", defaultEndpointId: "deepseek.openai",
    contextWindow: 1_000_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 35 },
  },
  // ── Kimi ───────────────────────────────────────────────────────────────────
  "kimi/moonshot-v1-8k": {
    id: "kimi/moonshot-v1-8k", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 8_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  "kimi/moonshot-v1-32k": {
    id: "kimi/moonshot-v1-32k", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 32_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "kimi/moonshot-v1-128k": {
    id: "kimi/moonshot-v1-128k", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 30 },
  },
  "kimi/kimi-k2.5": {
    id: "kimi/kimi-k2.5", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 30 },
  },
  "kimi/kimi-k2.6": {
    id: "kimi/kimi-k2.6", providerId: "kimi", defaultEndpointId: "kimi.openai",
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  // ── Qwen ───────────────────────────────────────────────────────────────────
  "qwen/qwen-max": {
    id: "qwen/qwen-max", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 32_768,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "qwen/qwen-plus": {
    id: "qwen/qwen-plus", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "qwen/qwq-plus": {
    id: "qwen/qwq-plus", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 40 },
  },
  "qwen/qwen3-235b-a22b": {
    id: "qwen/qwen3-235b-a22b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "qwen/qwen3-72b": {
    id: "qwen/qwen3-72b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "qwen/qwen3-32b": {
    id: "qwen/qwen3-32b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  // ── Gemini ─────────────────────────────────────────────────────────────────
  "gemini/gemini-2.5-pro": {
    id: "gemini/gemini-2.5-pro", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "gemini/gemini-2.5-flash": {
    id: "gemini/gemini-2.5-flash", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "gemini/gemini-2.0-flash": {
    id: "gemini/gemini-2.0-flash", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  "gemini/gemini-2.0-flash-lite": {
    id: "gemini/gemini-2.0-flash-lite", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 10 },
  },
  "gemini/gemini-1.5-pro": {
    id: "gemini/gemini-1.5-pro", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 2_097_152,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 30 },
  },
  "gemini/gemini-1.5-flash": {
    id: "gemini/gemini-1.5-flash", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
} as const satisfies Record<string, ModelProfile>

export type ModelProfileId = keyof typeof modelProfiles

export function getModelProfile(id: ModelProfileId): ModelProfile {
  return modelProfiles[id]
}
