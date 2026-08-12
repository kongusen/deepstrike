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
import {
  resolveCredential,
  resolveCredentialSync,
  type CredentialOptions,
  type ProviderCredential,
} from "./credentials.js"
import type { ModelCatalog } from "./model-catalog.js"

export type { EndpointProfileId } from "./endpoints.js"

export interface CreateProviderOptions {
  model: string
  /** Explicit credential is kept source-compatible; resolver-backed construction can omit it. */
  apiKey?: string
  bearerToken?: string
  credentialResolver?: CredentialOptions["credentialResolver"]
  /** Host-provided catalog used only for lookup; it never carries routing policy. */
  modelCatalog?: ModelCatalog
  provider?: ProviderId
  endpoint?: EndpointProfileId
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
}

export function createProvider(options: CreateProviderOptions): LLMProvider {
  return resolveProviderRuntime(options).adapter
}

export function resolveProviderRuntime(options: CreateProviderOptions): ResolvedProviderRuntime {
  const draft = resolveRuntimeDraft(options)
  const credential = draft.providerId === "ollama"
    ? { type: "api_key" as const, value: "" }
    : resolveCredentialSync(draft.credentialRequest, options)
  return constructResolvedRuntime(draft, credential, options)
}

/** I/O-capable equivalent for host credential resolvers and dynamic catalogs. */
export async function resolveProviderRuntimeAsync(options: CreateProviderOptions): Promise<ResolvedProviderRuntime> {
  const draft = await resolveRuntimeDraftAsync(options)
  const credential = draft.providerId === "ollama"
    ? { type: "api_key" as const, value: "" }
    : await resolveCredential(draft.credentialRequest, options)
  return constructResolvedRuntime(draft, credential, options)
}

export async function createProviderAsync(options: CreateProviderOptions): Promise<LLMProvider> {
  return (await resolveProviderRuntimeAsync(options)).adapter
}

interface RuntimeDraft {
  providerId: ProviderId
  model: string
  endpointId: EndpointProfileId
  endpoint: (typeof endpointProfiles)[EndpointProfileId]
  registration: NonNullable<ReturnType<typeof modelRegistry.resolve>>
  credentialRequest: {
    providerId: ProviderId
    modelId: string
    endpointId: EndpointProfileId
    protocol: (typeof endpointProfiles)[EndpointProfileId]["protocol"]
  }
}

function resolveRuntimeDraft(options: CreateProviderOptions): RuntimeDraft {
  const parsedProviderId = providerPrefix(options.model)
  const providerHint = options.provider ?? parsedProviderId
  const initialRegistration = modelRegistry.resolve(options.model, providerHint)
  return resolveRuntimeDraftWithRegistration(options, parsedProviderId, initialRegistration)
}

async function resolveRuntimeDraftAsync(options: CreateProviderOptions): Promise<RuntimeDraft> {
  const parsedProviderId = providerPrefix(options.model)
  const providerHint = options.provider ?? parsedProviderId
  const fromCatalog = options.modelCatalog ? await options.modelCatalog.get(options.model) : undefined
  const initialRegistration = fromCatalog ?? modelRegistry.resolve(options.model, providerHint)
  return resolveRuntimeDraftWithRegistration(options, parsedProviderId, initialRegistration)
}

function resolveRuntimeDraftWithRegistration(
  options: CreateProviderOptions,
  parsedProviderId: ProviderId | undefined,
  initialRegistration: ReturnType<typeof modelRegistry.resolve>,
): RuntimeDraft {
  const providerHint = options.provider ?? parsedProviderId
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
  return {
    providerId,
    model,
    endpointId,
    endpoint,
    registration,
    credentialRequest: { providerId, modelId: model, endpointId, protocol: endpoint.protocol },
  }
}

function constructResolvedRuntime(
  draft: RuntimeDraft,
  credential: ProviderCredential,
  options: CreateProviderOptions,
): ResolvedProviderRuntime {
  const make = PROVIDER_REGISTRY[providerRegistryKey(draft.providerId, draft.endpoint.protocol)]
  if (!make) throw new Error(`No Node provider factory for ${draft.model} on ${draft.endpoint.id}`)
  const protocol = generationProtocol(draft.endpoint.protocol)
  if (!protocol) throw new Error(`No Node provider factory for ${draft.model} on ${draft.endpoint.id}`)
  const adapter = make(
    credential.value,
    draft.model,
    options.retry,
    options.baseURL ?? draft.endpoint.baseURL,
    draft.registration.recommendedRuntimePolicy,
    credential.type,
  )
  const resolved: ResolvedProviderRuntime = {
    identity: { providerId: draft.providerId, modelId: draft.model, endpointId: draft.endpointId, protocol },
    model: draft.registration.descriptor,
    endpoint: draft.endpoint,
    adapter,
    effectiveCapabilities: resolveEffectiveModelCapabilities({
      model: draft.registration.descriptor,
      protocol,
      endpointCapabilities: endpointCapabilitiesFor(
        draft.endpointId,
        options.baseURL === undefined || options.endpoint !== undefined,
      ),
    }),
    ...(draft.registration.recommendedRuntimePolicy ? { runtimePolicy: draft.registration.recommendedRuntimePolicy } : {}),
  }
  const bind = (adapter as LLMProvider & { bindResolvedRuntime?: (runtime: ResolvedProviderRuntime) => void }).bindResolvedRuntime
  bind?.call(adapter, resolved)
  return resolved
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
