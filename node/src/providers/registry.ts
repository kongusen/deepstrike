import type { LLMProvider, RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { OpenAIResponsesProvider } from "./openai-responses.js"
import { DeepSeekAnthropicProvider } from "./deepseek.js"
import { KimiAnthropicProvider } from "./kimi.js"
import { QwenAnthropicProvider } from "./qwen.js"
import { GLMAnthropicProvider } from "./glm.js"
import { MiniMaxAnthropicProvider } from "./minimax.js"
import { GeminiProvider } from "./gemini.js"
import { OllamaProvider } from "./ollama.js"
import { openAIChatDialects, type OpenAIChatDialectId } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"

export type ProviderRetry = { maxRetries: number; baseDelay: number }

/** Constructs a provider for one `(providerId, endpointProtocol)` pair. The lambda absorbs the
 *  per-class constructor-shape differences (e.g. AnthropicProvider takes a `{ baseURL }` options
 *  object, the rest take a positional `baseURL` string). */
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

function openAIChatMaker(dialectId: OpenAIChatDialectId): ProviderMaker {
  const dialect = openAIChatDialects[dialectId]
  return (apiKey, model, retry, baseURL, runtimePolicy, authMode) => new OpenAIChatProvider(
    apiKey,
    model,
    retry,
    baseURL ?? endpointProfiles[dialect.endpointId].baseURL,
    runtimePolicy,
    dialect,
    authMode,
  )
}

const openAIChatRegistry = Object.fromEntries(
  Object.values(openAIChatDialects).map(dialect => [
    providerRegistryKey(dialect.providerId, "openai-chat"),
    openAIChatMaker(dialect.id as OpenAIChatDialectId),
  ]),
) as Record<string, ProviderMaker>

/**
 * Single source of truth for which provider class backs each `(vendor, wire)` pair. Consumed by
 * both `createProvider` (catalog) and the per-backend factory functions, so the two can no longer
 * drift. Adding a vendor/wire = add a row here (+ its `vendor-profiles` / `endpointProfiles` data) —
 * no dispatch branch to edit. OpenAI-compatible rows are generated from `openAIChatDialects`;
 * deprecated named classes are constructor shims only and are not runtime registry entries.
 */
export const PROVIDER_REGISTRY: Record<string, ProviderMaker> = {
  "anthropic:anthropic-messages": (k, m, r, b, p, authMode) => new AnthropicProvider(k, m, r, {
    baseURL: b,
    ...(p ? { runtimePolicy: p } : {}),
    ...(authMode === "bearer" ? { authMode: "bearer" as const } : {}),
  }),
  "openai:openai-responses":      (k, m, r, b, p, authMode) => new OpenAIResponsesProvider(k, m, r, b, p, authMode),

  "deepseek:anthropic-messages":  (k, m, r, b, p) => new DeepSeekAnthropicProvider(k, m, r, b, p),

  "kimi:anthropic-messages":      (k, m, r, b, p) => new KimiAnthropicProvider(k, m, r, b, p),

  "qwen:anthropic-messages":      (k, m, r, b, p) => new QwenAnthropicProvider(k, m, r, b, p),

  "glm:anthropic-messages":       (k, m, r, b, p) => new GLMAnthropicProvider(k, m, r, b, p),

  "minimax:anthropic-messages":   (k, m, r, b, p) => new MiniMaxAnthropicProvider(k, m, r, b, p),

  "gemini:gemini":                (k, m, r, b, p) => new GeminiProvider(k, m, r, b, p),
  "ollama:ollama-chat":            (_k, m, _r, b, p) => new OllamaProvider(m, b, p),
  ...openAIChatRegistry,
}
