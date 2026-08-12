export type ProviderId =
  | "anthropic"
  | "openai"
  | "minimax"
  | "deepseek"
  | "kimi"
  | "qwen"
  | "gemini"
  | "glm"
  | "baai"
  | "ollama"

export type EndpointProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "openai-embeddings"
  | "dashscope-multimodal-embeddings"
  | "gemini"
  | "gemini-embeddings"
  | "self-hosted-embeddings"
  | "ollama-chat"

export interface EndpointProfile {
  id: string
  providerId: ProviderId
  protocol: EndpointProtocol
  baseURL: string
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
  "deepseek.anthropic": {
    id: "deepseek.anthropic",
    providerId: "deepseek",
    protocol: "anthropic-messages",
    baseURL: "https://api.deepseek.com/anthropic",
  },
  "deepseek.openai": {
    id: "deepseek.openai",
    providerId: "deepseek",
    protocol: "openai-chat",
    baseURL: "https://api.deepseek.com",
  },
  "kimi.anthropic": {
    id: "kimi.anthropic",
    providerId: "kimi",
    protocol: "anthropic-messages",
    baseURL: "https://api.moonshot.cn/anthropic",
  },
  "kimi.openai": {
    id: "kimi.openai",
    providerId: "kimi",
    protocol: "openai-chat",
    baseURL: "https://api.moonshot.cn/v1",
  },
  "qwen.anthropic": {
    id: "qwen.anthropic",
    providerId: "qwen",
    protocol: "anthropic-messages",
    baseURL: "https://dashscope-intl.aliyuncs.com/apps/anthropic",
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
  "glm.anthropic": {
    id: "glm.anthropic",
    providerId: "glm",
    protocol: "anthropic-messages",
    baseURL: "https://open.bigmodel.cn/api/anthropic",
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
  "ollama.local": {
    id: "ollama.local",
    providerId: "ollama",
    protocol: "ollama-chat",
    baseURL: "http://localhost:11434",
  },
} as const satisfies Record<string, EndpointProfile>

export type EndpointProfileId = keyof typeof endpointProfiles
