import type { LLMProvider } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { DeepSeekProvider } from "./deepseek.js"
import { KimiProvider } from "./kimi.js"
import { OpenAIResponsesProvider } from "./openai-responses.js"
import { MiniMaxProvider } from "./minimax.js"
import { QwenProvider } from "./qwen.js"
import { GeminiProvider } from "./gemini.js"
import { endpointProfiles, getModelProfile, type ModelProfileId } from "./profiles.js"

export type EndpointProfileId = keyof typeof endpointProfiles

export interface CreateProviderOptions {
  model: ModelProfileId
  apiKey: string
  endpoint?: EndpointProfileId
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
}

export function createProvider(options: CreateProviderOptions): LLMProvider {
  const profile = getModelProfile(options.model)
  const endpointId = (options.endpoint ?? profile.defaultEndpointId) as EndpointProfileId
  const endpoint = endpointProfiles[endpointId]

  if (!endpoint) {
    throw new Error(`Unknown endpoint profile: ${endpointId}`)
  }
  if (endpoint.providerId !== profile.providerId) {
    throw new Error(`Endpoint ${endpoint.id} does not belong to provider ${profile.providerId}`)
  }

  const model = options.model.slice(`${profile.providerId}/`.length)
  const baseURL = options.baseURL ?? endpoint.baseURL

  if (profile.providerId === "openai") {
    if (endpoint.protocol === "openai-chat") {
      return new OpenAIChatProvider(options.apiKey, model, options.retry, baseURL)
    }
    if (endpoint.protocol === "openai-responses") {
      return new OpenAIResponsesProvider(options.apiKey, model, options.retry, baseURL)
    }
  }
  if (profile.providerId === "minimax" && endpoint.protocol === "anthropic-messages") {
    return new MiniMaxProvider(options.apiKey, model as "MiniMax-M2.7" | "MiniMax-M2.5", options.retry, baseURL)
  }
  if (profile.providerId === "deepseek" && endpoint.protocol === "openai-chat") {
    return new DeepSeekProvider(options.apiKey, model as "deepseek-v4-flash" | "deepseek-v4-pro", options.retry, baseURL)
  }
  if (profile.providerId === "kimi" && endpoint.protocol === "openai-chat") {
    return new KimiProvider(options.apiKey, model as "kimi-k2.5" | "kimi-k2.6", options.retry, baseURL)
  }
  if (profile.providerId === "qwen" && endpoint.protocol === "openai-chat") {
    return new QwenProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (profile.providerId === "gemini" && endpoint.protocol === "gemini") {
    return new GeminiProvider(options.apiKey, model, options.retry, baseURL)
  }

  throw new Error(`No Node provider factory for ${profile.id} on ${endpoint.id}`)
}
