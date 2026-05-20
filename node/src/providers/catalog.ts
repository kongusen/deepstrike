import type { LLMProvider } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { DeepSeekProvider } from "./deepseek.js"
import { KimiProvider } from "./kimi.js"
import { OpenAIResponsesProvider } from "./openai-responses.js"
import { MiniMaxProvider } from "./minimax.js"
import { QwenProvider } from "./qwen.js"
import { GeminiProvider } from "./gemini.js"
import { GLMProvider } from "./glm.js"
import { endpointProfiles, getModelProfile, modelProfiles, type ModelProfileId, type ProviderId } from "./profiles.js"

export type EndpointProfileId = keyof typeof endpointProfiles

export interface CreateProviderOptions {
  model: ModelProfileId | string
  apiKey: string
  provider?: ProviderId
  endpoint?: EndpointProfileId
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
}

export function createProvider(options: CreateProviderOptions): LLMProvider {
  const profile = isModelProfileId(options.model) ? getModelProfile(options.model) : undefined
  const parsedProviderId = providerPrefix(options.model)
  const endpointId = (options.endpoint ?? profile?.defaultEndpointId ?? defaultEndpointForProvider(options.provider ?? parsedProviderId)) as EndpointProfileId | undefined

  if (!endpointId) {
    throw new Error(`Unknown model profile: ${options.model}. Pass provider or endpoint for custom model names.`)
  }

  const endpoint = endpointProfiles[endpointId]

  if (!endpoint) {
    throw new Error(`Unknown endpoint profile: ${endpointId}`)
  }

  const providerId = profile?.providerId ?? options.provider ?? parsedProviderId ?? endpoint.providerId
  if (profile && options.provider && options.provider !== profile.providerId) {
    throw new Error(`Model ${profile.id} belongs to provider ${profile.providerId}, not ${options.provider}`)
  }
  if (parsedProviderId && options.provider && parsedProviderId !== options.provider) {
    throw new Error(`Model ${options.model} uses provider prefix ${parsedProviderId}, not ${options.provider}`)
  }
  if (endpoint.providerId !== providerId) {
    throw new Error(`Endpoint ${endpoint.id} does not belong to provider ${providerId}`)
  }

  const model = modelNameForProvider(options.model, providerId)
  const baseURL = options.baseURL ?? endpoint.baseURL

  if (providerId === "anthropic" && endpoint.protocol === "anthropic-messages") {
    return new AnthropicProvider(options.apiKey, model, options.retry, { baseURL })
  }
  if (providerId === "openai") {
    if (endpoint.protocol === "openai-chat") {
      return new OpenAIChatProvider(options.apiKey, model, options.retry, baseURL)
    }
    if (endpoint.protocol === "openai-responses") {
      return new OpenAIResponsesProvider(options.apiKey, model, options.retry, baseURL)
    }
  }
  if (providerId === "minimax" && endpoint.protocol === "anthropic-messages") {
    return new MiniMaxProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (providerId === "deepseek" && endpoint.protocol === "openai-chat") {
    return new DeepSeekProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (providerId === "kimi" && endpoint.protocol === "openai-chat") {
    return new KimiProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (providerId === "qwen" && endpoint.protocol === "openai-chat") {
    return new QwenProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (providerId === "gemini" && endpoint.protocol === "gemini") {
    return new GeminiProvider(options.apiKey, model, options.retry, baseURL)
  }
  if (providerId === "glm" && endpoint.protocol === "openai-chat") {
    return new GLMProvider(options.apiKey, model, options.retry, baseURL)
  }

  throw new Error(`No Node provider factory for ${options.model} on ${endpoint.id}`)
}

function isModelProfileId(model: string): model is ModelProfileId {
  return Object.prototype.hasOwnProperty.call(modelProfiles, model)
}

function providerPrefix(model: string): ProviderId | undefined {
  const [prefix] = model.split("/", 1)
  return providerIds().includes(prefix as ProviderId) ? prefix as ProviderId : undefined
}

function providerIds(): ProviderId[] {
  return Array.from(new Set(Object.values(endpointProfiles).map(endpoint => endpoint.providerId)))
}

function defaultEndpointForProvider(providerId: ProviderId | undefined): EndpointProfileId | undefined {
  if (!providerId) return undefined
  const defaults: Partial<Record<ProviderId, EndpointProfileId>> = {
    anthropic: "anthropic.messages",
    openai: "openai.chat",
    minimax: "minimax.anthropic",
    deepseek: "deepseek.openai",
    kimi: "kimi.openai",
    qwen: "qwen.dashscope",
    gemini: "gemini.google",
    glm: "glm.openai",
  }
  return defaults[providerId]
}

function modelNameForProvider(model: string, providerId: ProviderId): string {
  const prefix = `${providerId}/`
  return model.startsWith(prefix) ? model.slice(prefix.length) : model
}
