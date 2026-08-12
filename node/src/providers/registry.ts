import type { LLMProvider, RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { OpenAIResponsesProvider } from "./openai-responses.js"
import { DeepSeekProvider, DeepSeekAnthropicProvider } from "./deepseek.js"
import { KimiProvider, KimiAnthropicProvider } from "./kimi.js"
import { QwenProvider, QwenAnthropicProvider } from "./qwen.js"
import { GLMProvider, GLMAnthropicProvider } from "./glm.js"
import { MiniMaxOpenAIProvider, MiniMaxAnthropicProvider } from "./minimax.js"
import { GeminiProvider } from "./gemini.js"
import { OllamaProvider } from "./ollama.js"

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
) => LLMProvider

/** Build the registry key for a `(providerId, endpointProtocol)` pair. */
export function providerRegistryKey(providerId: string, protocol: string): string {
  return `${providerId}:${protocol}`
}

/**
 * Single source of truth for which provider class backs each `(vendor, wire)` pair. Consumed by
 * both `createProvider` (catalog) and the per-backend factory functions, so the two can no longer
 * drift. Adding a vendor/wire = add a row here (+ its `vendor-profiles` / `endpointProfiles` data) —
 * no dispatch branch to edit. Values are the same named classes as before, so `instanceof` holds.
 */
export const PROVIDER_REGISTRY: Record<string, ProviderMaker> = {
  "anthropic:anthropic-messages": (k, m, r, b, p) => new AnthropicProvider(k, m, r, { baseURL: b, ...(p ? { runtimePolicy: p } : {}) }),
  "openai:openai-chat":           (k, m, r, b, p) => new OpenAIChatProvider(k, m, r, b, p),
  "openai:openai-responses":      (k, m, r, b, p) => new OpenAIResponsesProvider(k, m, r, b, p),

  "deepseek:openai-chat":         (k, m, r, b, p) => new DeepSeekProvider(k, m, r, b, p),
  "deepseek:anthropic-messages":  (k, m, r, b, p) => new DeepSeekAnthropicProvider(k, m, r, b, p),

  "kimi:openai-chat":             (k, m, r, b, p) => new KimiProvider(k, m, r, b, p),
  "kimi:anthropic-messages":      (k, m, r, b, p) => new KimiAnthropicProvider(k, m, r, b, p),

  "qwen:openai-chat":             (k, m, r, b, p) => new QwenProvider(k, m, r, b, p),
  "qwen:anthropic-messages":      (k, m, r, b, p) => new QwenAnthropicProvider(k, m, r, b, p),

  "glm:openai-chat":              (k, m, r, b, p) => new GLMProvider(k, m, r, b, p),
  "glm:anthropic-messages":       (k, m, r, b, p) => new GLMAnthropicProvider(k, m, r, b, p),

  "minimax:openai-chat":          (k, m, r, b, p) => new MiniMaxOpenAIProvider(k, m, r, b, p),
  "minimax:anthropic-messages":   (k, m, r, b, p) => new MiniMaxAnthropicProvider(k, m, r, b, p),

  "gemini:gemini":                (k, m, r, b, p) => new GeminiProvider(k, m, r, b, p),
  "ollama:ollama-chat":            (_k, m, _r, b, p) => new OllamaProvider(m, b, p),
}
