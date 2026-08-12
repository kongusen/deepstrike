import type { LLMProvider } from "../types.js"
import { PROVIDER_REGISTRY, providerRegistryKey } from "./registry.js"
import {
  endpointCapabilitiesFor,
  generationProtocol,
  modelRegistry,
  normalizeModelId,
  resolveEffectiveModelCapabilities,
  type ResolvedProviderRuntime,
} from "./model-registry.js"
import { endpointProfiles, type EndpointProfileId, type ProviderId } from "./endpoints.js"

export type { EndpointProfileId } from "./endpoints.js"

export interface CreateProviderOptions {
  model: string
  apiKey: string
  provider?: ProviderId
  endpoint?: EndpointProfileId
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
}

export function createProvider(options: CreateProviderOptions): LLMProvider {
  return resolveProviderRuntime(options).adapter
}

export function resolveProviderRuntime(options: CreateProviderOptions): ResolvedProviderRuntime {
  const parsedProviderId = providerPrefix(options.model)
  const providerHint = options.provider ?? parsedProviderId
  const initialRegistration = modelRegistry.resolve(options.model, providerHint)
  const endpointId = (options.endpoint ?? initialRegistration?.defaultEndpointId ?? defaultEndpointForProvider(providerHint)) as EndpointProfileId | undefined

  if (!endpointId) {
    throw new Error(`Unknown model profile: ${options.model}. Pass provider or endpoint for custom model names.`)
  }

  const endpoint = endpointProfiles[endpointId]

  if (!endpoint) {
    throw new Error(`Unknown endpoint profile: ${endpointId}`)
  }

  const providerId = (initialRegistration?.descriptor.providerId ?? options.provider ?? parsedProviderId ?? endpoint.providerId) as ProviderId
  const registration = initialRegistration ?? modelRegistry.resolve(options.model, providerId)
  if (!registration) throw new Error(`Unable to resolve model ${options.model} for provider ${providerId}`)
  if (parsedProviderId && options.provider && parsedProviderId !== options.provider) {
    throw new Error(`Model ${options.model} uses provider prefix ${parsedProviderId}, not ${options.provider}`)
  }
  if (endpoint.providerId !== providerId) {
    throw new Error(`Endpoint ${endpoint.id} does not belong to provider ${providerId}`)
  }

  const model = normalizeModelId(providerId, options.model)
  const baseURL = options.baseURL ?? endpoint.baseURL

  // Single data-driven dispatch: one registry keyed by (providerId, protocol).
  const make = PROVIDER_REGISTRY[providerRegistryKey(providerId, endpoint.protocol)]
  if (make) {
    const protocol = generationProtocol(endpoint.protocol)
    if (!protocol) throw new Error(`No Node provider factory for ${options.model} on ${endpoint.id}`)
    const adapter = make(options.apiKey, model, options.retry, baseURL, registration.recommendedRuntimePolicy)
    const preserveEndpointIdentity = options.baseURL === undefined || options.endpoint !== undefined
    return {
      identity: {
        providerId,
        modelId: model,
        endpointId,
        protocol,
      },
      model: registration?.descriptor,
      endpoint,
      adapter,
      effectiveCapabilities: resolveEffectiveModelCapabilities({
        model: registration?.descriptor,
        protocol,
        endpointCapabilities: endpointCapabilitiesFor(endpointId, preserveEndpointIdentity),
      }),
      ...(registration?.recommendedRuntimePolicy
        ? { runtimePolicy: registration.recommendedRuntimePolicy }
        : {}),
    }
  }

  throw new Error(`No Node provider factory for ${options.model} on ${endpoint.id}`)
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
    deepseek: "deepseek.anthropic",
    kimi: "kimi.anthropic",
    qwen: "qwen.anthropic",
    gemini: "gemini.google",
    glm: "glm.anthropic",
    baai: "baai.self-hosted.embeddings",
    ollama: "ollama.local",
  }
  return defaults[providerId]
}
