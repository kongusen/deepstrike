import type { LLMProvider, RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { OpenAIResponsesProvider } from "./openai-responses.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { GeminiProvider } from "./gemini.js"
import { OllamaProvider } from "./ollama.js"
import { openAIChatDialects, type OpenAIChatDialectId } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import type { EndpointProtocol, ProviderId } from "./endpoints.js"
import { anthropicVendorProfiles, type AnthropicVendorId } from "./vendor-profiles.js"

export type ProviderRetry = { maxRetries: number; baseDelay: number }

/** Constructs a provider for one `(providerId, endpointProtocol)` pair. */
export type ProviderMaker = (
  apiKey: string,
  model: string | undefined,
  retry: ProviderRetry | undefined,
  baseURL: string | undefined,
  runtimePolicy: RuntimePolicy | undefined,
  authMode?: "api_key" | "bearer",
) => LLMProvider

/** Build the registry key for a `(providerId, endpointProtocol)` pair. */
export function providerRegistryKey(providerId: string, protocol: string): string {
  return `${providerId}:${protocol}`
}

/** OAuth bearer is opt-in per provider/protocol. Compatible endpoints do not inherit this policy. */
export function supportsBearerCredential(providerId: ProviderId, protocol: EndpointProtocol): boolean {
  return (providerId === "openai" && (protocol === "openai-chat" || protocol === "openai-responses"))
    || (providerId === "anthropic" && protocol === "anthropic-messages")
}

function openAIChatMaker(dialectId: OpenAIChatDialectId): ProviderMaker {
  const dialect = openAIChatDialects[dialectId]
  return (apiKey, model, retry, baseURL, runtimePolicy, authMode) => new OpenAIChatProvider({
    apiKey,
    model,
    retry,
    baseURL: baseURL ?? endpointProfiles[dialect.endpointId].baseURL,
    runtimePolicy,
    dialect,
    authMode,
  })
}

const openAIChatRegistry = Object.fromEntries(
  Object.values(openAIChatDialects).map(dialect => [
    providerRegistryKey(dialect.providerId, "openai-chat"),
    openAIChatMaker(dialect.id as OpenAIChatDialectId),
  ]),
) as Record<string, ProviderMaker>

function anthropicCompatibleMaker(vendorId: AnthropicVendorId): ProviderMaker {
  return (apiKey, model, retry, baseURL, runtimePolicy) => new AnthropicCompatibleProvider(
    anthropicVendorProfiles[vendorId],
    apiKey,
    model,
    retry,
    baseURL,
    runtimePolicy,
  )
}

/**
 * Single source of truth for which provider class backs each `(vendor, wire)` pair. Consumed by
 * both `createProvider` (catalog) and the per-backend factory functions, so the two can no longer
 * drift. Adding a vendor/wire = add a row here (+ its `vendor-profiles` / `endpointProfiles` data) —
 * no dispatch branch to edit. Compatible rows are generated from dialect/profile data.
 */
export const PROVIDER_REGISTRY: Record<string, ProviderMaker> = {
  "anthropic:anthropic-messages": (k, m, r, b, p, authMode) => new AnthropicProvider({
    apiKey: k,
    model: m,
    retry: r,
    baseURL: b,
    ...(p ? { runtimePolicy: p } : {}),
    ...(authMode === "bearer" ? { authMode: "bearer" as const } : {}),
  }),
  "openai:openai-responses":      (k, m, r, b, p, authMode) => new OpenAIResponsesProvider(k, m, r, b, p, authMode),

  "deepseek:anthropic-messages":  anthropicCompatibleMaker("deepseek"),
  "kimi:anthropic-messages":      anthropicCompatibleMaker("kimi"),
  "qwen:anthropic-messages":      anthropicCompatibleMaker("qwen"),
  "glm:anthropic-messages":       anthropicCompatibleMaker("glm"),
  "minimax:anthropic-messages":   anthropicCompatibleMaker("minimax"),

  "gemini:gemini":                (k, m, r, b, p) => new GeminiProvider(k, m, r, b, p),
  "ollama:ollama-chat":            (_k, m, _r, b, p) => new OllamaProvider(m, b, p),
  ...openAIChatRegistry,
}
