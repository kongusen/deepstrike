export type ProviderId = "anthropic" | "openai" | "minimax" | "deepseek" | "kimi" | "qwen" | "gemini" | "glm" | "baai"
export type EndpointProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "openai-embeddings"
  | "dashscope-multimodal-embeddings"
  | "gemini"
  | "gemini-embeddings"
  | "self-hosted-embeddings"

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
    input: Array<"text" | "image" | "audio" | "video" | "pdf">
    output: Array<"text" | "embedding">
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
  "openai.embeddings": {
    id: "openai.embeddings",
    providerId: "openai",
    protocol: "openai-embeddings",
    baseURL: "https://api.openai.com/v1",
  },
  "minimax.anthropic": {
    id: "minimax.anthropic",
    providerId: "minimax",
    protocol: "anthropic-messages",
    baseURL: "https://api.minimaxi.com/anthropic",
  },
  "minimax.openai": {
    id: "minimax.openai",
    providerId: "minimax",
    protocol: "openai-chat",
    baseURL: "https://api.minimaxi.com/v1",
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
  "qwen.dashscope.embeddings": {
    id: "qwen.dashscope.embeddings",
    providerId: "qwen",
    protocol: "openai-embeddings",
    baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1",
  },
  "qwen.dashscope.multimodal-embeddings": {
    id: "qwen.dashscope.multimodal-embeddings",
    providerId: "qwen",
    protocol: "dashscope-multimodal-embeddings",
    baseURL: "https://dashscope.aliyuncs.com/api/v1/services/embeddings/multimodal-embedding/multimodal-embedding",
  },
  "gemini.google": {
    id: "gemini.google",
    providerId: "gemini",
    protocol: "gemini",
    baseURL: "https://generativelanguage.googleapis.com",
  },
  "gemini.google.embeddings": {
    id: "gemini.google.embeddings",
    providerId: "gemini",
    protocol: "gemini-embeddings",
    baseURL: "https://generativelanguage.googleapis.com",
  },
  "glm.openai": {
    id: "glm.openai",
    providerId: "glm",
    protocol: "openai-chat",
    baseURL: "https://open.bigmodel.cn/api/paas/v4",
  },
  "glm.openai.embeddings": {
    id: "glm.openai.embeddings",
    providerId: "glm",
    protocol: "openai-embeddings",
    baseURL: "https://open.bigmodel.cn/api/paas/v4",
  },
  "baai.self-hosted.embeddings": {
    id: "baai.self-hosted.embeddings",
    providerId: "baai",
    protocol: "self-hosted-embeddings",
    baseURL: "https://huggingface.co/BAAI",
  },
} as const satisfies Record<string, EndpointProfile>

export const modelProfiles = {
  "anthropic/claude-opus-4-1": {
    id: "anthropic/claude-opus-4-1",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
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
  "anthropic/claude-opus-4-0": {
    id: "anthropic/claude-opus-4-0",
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
  "anthropic/claude-sonnet-4-0": {
    id: "anthropic/claude-sonnet-4-0",
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
  "anthropic/claude-3-5-haiku-latest": {
    id: "anthropic/claude-3-5-haiku-latest",
    providerId: "anthropic",
    defaultEndpointId: "anthropic.messages",
    contextWindow: 200_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true },
    reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  // ── OpenAI ─────────────────────────────────────────────────────────────────
  "openai/gpt-5.5": {
    id: "openai/gpt-5.5", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 60 },
  },
  "openai/gpt-5.4": {
    id: "openai/gpt-5.4", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 1_050_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "openai/gpt-5.4-mini": {
    id: "openai/gpt-5.4-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 400_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "openai/gpt-5.4-nano": {
    id: "openai/gpt-5.4-nano", providerId: "openai", defaultEndpointId: "openai.responses",
    contextWindow: 400_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 15 },
  },
  "openai/gpt-5.2": {
    id: "openai/gpt-5.2", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "openai/gpt-5.2-pro": {
    id: "openai/gpt-5.2-pro", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 60 },
  },
  "openai/gpt-5.1": {
    id: "openai/gpt-5.1", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
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
  "openai/gpt-5-pro": {
    id: "openai/gpt-5-pro", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 60 },
  },
  "openai/gpt-5-mini": {
    id: "openai/gpt-5-mini", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "openai/gpt-5-nano": {
    id: "openai/gpt-5-nano", providerId: "openai", defaultEndpointId: "openai.responses",
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 15 },
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
  "openai/text-embedding-3-large": {
    id: "openai/text-embedding-3-large", providerId: "openai", defaultEndpointId: "openai.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "openai/text-embedding-3-small": {
    id: "openai/text-embedding-3-small", providerId: "openai", defaultEndpointId: "openai.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "openai/text-embedding-ada-002": {
    id: "openai/text-embedding-ada-002", providerId: "openai", defaultEndpointId: "openai.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  // ── MiniMax ────────────────────────────────────────────────────────────────
  "minimax/MiniMax-M2.7": {
    id: "minimax/MiniMax-M2.7", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 35 },
  },
  "minimax/MiniMax-M2.7-highspeed": {
    id: "minimax/MiniMax-M2.7-highspeed", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
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
  "minimax/MiniMax-M2.5-highspeed": {
    id: "minimax/MiniMax-M2.5-highspeed", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "minimax/MiniMax-M2.1": {
    id: "minimax/MiniMax-M2.1", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "minimax/MiniMax-M2.1-highspeed": {
    id: "minimax/MiniMax-M2.1-highspeed", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 25 },
  },
  "minimax/MiniMax-M2": {
    id: "minimax/MiniMax-M2", providerId: "minimax", defaultEndpointId: "minimax.anthropic",
    contextWindow: 204_800,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 20 },
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
  "kimi/kimi-k2-thinking": {
    id: "kimi/kimi-k2-thinking", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 256_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "kimi/kimi-k2-thinking-turbo": {
    id: "kimi/kimi-k2-thinking-turbo", providerId: "kimi", defaultEndpointId: "kimi.openai",
    contextWindow: 256_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 40 },
  },
  // ── Qwen ───────────────────────────────────────────────────────────────────
  "qwen/qwen3.7-max-preview": {
    id: "qwen/qwen3.7-max-preview", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 45 },
  },
  "qwen/qwen3.7-plus-preview": {
    id: "qwen/qwen3.7-plus-preview", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 40 },
  },
  "qwen/qwen3.6-max-preview": {
    id: "qwen/qwen3.6-max-preview", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 40 },
  },
  "qwen/qwen3.6-plus": {
    id: "qwen/qwen3.6-plus", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "qwen/qwen3.6-flash": {
    id: "qwen/qwen3.6-flash", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "qwen/qwen3.6-35b-a3b": {
    id: "qwen/qwen3.6-35b-a3b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "qwen/qwen3.6-27b": {
    id: "qwen/qwen3.6-27b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "qwen/qwen3.5-plus": {
    id: "qwen/qwen3.5-plus", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "qwen/qwen3.5-flash": {
    id: "qwen/qwen3.5-flash", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 1_000_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "qwen/qwen3.5-397b-a17b": {
    id: "qwen/qwen3.5-397b-a17b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "qwen/qwen3.5-122b-a10b": {
    id: "qwen/qwen3.5-122b-a10b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "qwen/qwen3.5-35b-a3b": {
    id: "qwen/qwen3.5-35b-a3b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "qwen/qwen3.5-27b": {
    id: "qwen/qwen3.5-27b", providerId: "qwen", defaultEndpointId: "qwen.dashscope",
    contextWindow: 256_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "qwen/text-embedding-v4": {
    id: "qwen/text-embedding-v4", providerId: "qwen", defaultEndpointId: "qwen.dashscope.embeddings",
    contextWindow: 8_192,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "qwen/text-embedding-v3": {
    id: "qwen/text-embedding-v3", providerId: "qwen", defaultEndpointId: "qwen.dashscope.embeddings",
    contextWindow: 8_192,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "qwen/qwen3-vl-embedding": {
    id: "qwen/qwen3-vl-embedding", providerId: "qwen", defaultEndpointId: "qwen.dashscope.multimodal-embeddings",
    contextWindow: 32_000,
    modalities: { input: ["text", "image", "video"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "qwen/qwen2.5-vl-embedding": {
    id: "qwen/qwen2.5-vl-embedding", providerId: "qwen", defaultEndpointId: "qwen.dashscope.multimodal-embeddings",
    contextWindow: 32_000,
    modalities: { input: ["text", "image", "video"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  // ── Gemini ─────────────────────────────────────────────────────────────────
  "gemini/gemini-3-pro-preview": {
    id: "gemini/gemini-3-pro-preview", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 50 },
  },
  "gemini/gemini-3-flash-preview": {
    id: "gemini/gemini-3-flash-preview", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 25 },
  },
  "gemini/gemini-3.5-flash": {
    id: "gemini/gemini-3.5-flash", providerId: "gemini", defaultEndpointId: "gemini.google",
    contextWindow: 1_048_576,
    modalities: { input: ["text", "image", "audio"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: false },
    policy: { maxTurns: 30 },
  },
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
  "gemini/gemini-embedding-001": {
    id: "gemini/gemini-embedding-001", providerId: "gemini", defaultEndpointId: "gemini.google.embeddings",
    contextWindow: 2_048,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "gemini/gemini-embedding-2": {
    id: "gemini/gemini-embedding-2", providerId: "gemini", defaultEndpointId: "gemini.google.embeddings",
    contextWindow: 8_192,
    modalities: { input: ["text", "image", "audio", "video", "pdf"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  // ── GLM ────────────────────────────────────────────────────────────────────
  "glm/glm-5.1": {
    id: "glm/glm-5.1", providerId: "glm", defaultEndpointId: "glm.openai",
    contextWindow: 200_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: true, preserveAcrossToolTurns: true },
    policy: { maxTurns: 50 },
  },
  "glm/glm-4-plus": {
    id: "glm/glm-4-plus", providerId: "glm", defaultEndpointId: "glm.openai",
    contextWindow: 128_000,
    modalities: { input: ["text", "image"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 35 },
  },
  "glm/glm-4-flash": {
    id: "glm/glm-4-flash", providerId: "glm", defaultEndpointId: "glm.openai",
    contextWindow: 128_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 15 },
  },
  "glm/glm-4-air": {
    id: "glm/glm-4-air", providerId: "glm", defaultEndpointId: "glm.openai",
    contextWindow: 128_000,
    modalities: { input: ["text"], output: ["text"] },
    tools: { supported: true }, reasoning: { supported: false, preserveAcrossToolTurns: false },
    policy: { maxTurns: 20 },
  },
  "glm/embedding-3": {
    id: "glm/embedding-3", providerId: "glm", defaultEndpointId: "glm.openai.embeddings",
    contextWindow: 8_000,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "glm/embedding-2": {
    id: "glm/embedding-2", providerId: "glm", defaultEndpointId: "glm.openai.embeddings",
    contextWindow: 8_000,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  // ── BAAI / BGE ─────────────────────────────────────────────────────────────
  "baai/bge-m3": {
    id: "baai/bge-m3", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    contextWindow: 8_192,
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-large-en-v1.5": {
    id: "baai/bge-large-en-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-base-en-v1.5": {
    id: "baai/bge-base-en-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-small-en-v1.5": {
    id: "baai/bge-small-en-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-large-zh-v1.5": {
    id: "baai/bge-large-zh-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-base-zh-v1.5": {
    id: "baai/bge-base-zh-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-small-zh-v1.5": {
    id: "baai/bge-small-zh-v1.5", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-code-v1": {
    id: "baai/bge-code-v1", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-vl-v1.5-zs": {
    id: "baai/bge-vl-v1.5-zs", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text", "image"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
  "baai/bge-vl-v1.5-mmeb": {
    id: "baai/bge-vl-v1.5-mmeb", providerId: "baai", defaultEndpointId: "baai.self-hosted.embeddings",
    modalities: { input: ["text", "image"], output: ["embedding"] },
    tools: { supported: false }, reasoning: { supported: false, preserveAcrossToolTurns: false },
  },
} as const satisfies Record<string, ModelProfile>

export type ModelProfileId = keyof typeof modelProfiles

export function getModelProfile(id: ModelProfileId): ModelProfile {
  return modelProfiles[id]
}
