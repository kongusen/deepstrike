export type ProviderId = "openai" | "minimax" | "deepseek" | "kimi" | "qwen" | "gemini"
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
}

export const endpointProfiles = {
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
  "openai/gpt-4o": {
    id: "openai/gpt-4o",
    providerId: "openai",
    defaultEndpointId: "openai.chat",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "openai/gpt-4o-mini": {
    id: "openai/gpt-4o-mini",
    providerId: "openai",
    defaultEndpointId: "openai.chat",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "openai/gpt-5": {
    id: "openai/gpt-5",
    providerId: "openai",
    defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "openai/gpt-5-mini": {
    id: "openai/gpt-5-mini",
    providerId: "openai",
    defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "minimax/MiniMax-M2.7": {
    id: "minimax/MiniMax-M2.7",
    providerId: "minimax",
    defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "minimax/MiniMax-M2.5": {
    id: "minimax/MiniMax-M2.5",
    providerId: "minimax",
    defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "deepseek/deepseek-v4-flash": {
    id: "deepseek/deepseek-v4-flash",
    providerId: "deepseek",
    defaultEndpointId: "deepseek.openai",
    contextWindow: 1_000_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "deepseek/deepseek-v4-pro": {
    id: "deepseek/deepseek-v4-pro",
    providerId: "deepseek",
    defaultEndpointId: "deepseek.openai",
    contextWindow: 1_000_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
  },
  "kimi/kimi-k2.5": {
    id: "kimi/kimi-k2.5",
    providerId: "kimi",
    defaultEndpointId: "kimi.openai",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
  },
  "kimi/kimi-k2.6": {
    id: "kimi/kimi-k2.6",
    providerId: "kimi",
    defaultEndpointId: "kimi.openai",
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
  },
  "qwen/qwen-max": {
    id: "qwen/qwen-max",
    providerId: "qwen",
    defaultEndpointId: "qwen.dashscope",
    contextWindow: 32_768,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "qwen/qwen-plus": {
    id: "qwen/qwen-plus",
    providerId: "qwen",
    defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "qwen/qwq-plus": {
    id: "qwen/qwq-plus",
    providerId: "qwen",
    defaultEndpointId: "qwen.dashscope",
    contextWindow: 131_072,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
  },
  "gemini/gemini-2.0-flash": {
    id: "gemini/gemini-2.0-flash",
    providerId: "gemini",
    defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "gemini/gemini-2.5-flash": {
    id: "gemini/gemini-2.5-flash",
    providerId: "gemini",
    defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
  },
  "gemini/gemini-2.5-pro": {
    id: "gemini/gemini-2.5-pro",
    providerId: "gemini",
    defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: false },
  },
} as const satisfies Record<string, ModelProfile>

export type ModelProfileId = keyof typeof modelProfiles

export function getModelProfile(id: ModelProfileId): ModelProfile {
  return modelProfiles[id]
}
