// Per-backend provider factories — one function per backend, the public way to construct a
// non-OpenAI/Anthropic backend. Where a backend offers both an OpenAI- and an Anthropic-compatible
// wire, the `protocol` option selects it. All construction goes through the shared
// `PROVIDER_REGISTRY`, so the (backend, wire) → class mapping lives in exactly one place and cannot
// drift from `createProvider`. For OpenAI itself use the root `OpenAIProvider` /
// `OpenAIResponsesProvider`; for any model by id use `createProvider`.
import type { LLMProvider } from "../types.js"
import { PROVIDER_REGISTRY } from "./registry.js"
import { OllamaProvider } from "./ollama.js"
import { defaultEndpointForProvider, defaultModelForProvider, getRuntimePolicy, isKnownProviderId } from "./model-registry.js"
import { endpointProfiles, type ProviderId } from "./endpoints.js"

/** Options for a backend provider factory. `protocol` only applies to backends with both wires. */
export interface BackendProviderOptions {
  apiKey: string
  model?: string
  /** Override the endpoint base URL (defaults to the backend's profile for the chosen protocol). */
  baseURL?: string
  retry?: { maxRetries: number; baseDelay: number }
  /** Wire protocol for dual-protocol backends. Defaults per backend (see each factory). */
  protocol?: "openai" | "anthropic"
}

function build(providerId: ProviderId, o: BackendProviderOptions): LLMProvider {
  if (!isKnownProviderId(providerId)) throw new Error(`Unknown provider: ${providerId}`)
  const defaultProtocol = endpointProfiles[defaultEndpointForProvider(providerId)].protocol
  const protocol = o.protocol === "openai"
    ? "openai-chat"
    : o.protocol === "anthropic"
      ? "anthropic-messages"
      : defaultProtocol
  if (protocol !== "openai-chat" && protocol !== "anthropic-messages") {
    throw new Error(`Provider ${providerId} does not use an OpenAI- or Anthropic-compatible default`)
  }
  const model = o.model ?? defaultModelForProvider(providerId)
  return PROVIDER_REGISTRY[`${providerId}:${protocol}`](
    o.apiKey,
    model,
    o.retry,
    o.baseURL,
    getRuntimePolicy(providerId, model),
  )
}

/** DeepSeek. Defaults to the OpenAI-compatible wire (richer reasoning-replay handling). */
export function deepseek(o: BackendProviderOptions): LLMProvider {
  return build("deepseek", o)
}

/** Moonshot Kimi. Defaults to the OpenAI-compatible wire. */
export function kimi(o: BackendProviderOptions): LLMProvider {
  return build("kimi", o)
}

/** Alibaba Qwen / DashScope. Defaults to the OpenAI-compatible (DashScope) wire. */
export function qwen(o: BackendProviderOptions): LLMProvider {
  return build("qwen", o)
}

/** Zhipu GLM. Defaults to the OpenAI-compatible wire. */
export function glm(o: BackendProviderOptions): LLMProvider {
  return build("glm", o)
}

/** MiniMax. Defaults to the Anthropic-compatible wire (the primary M2.x path). */
export function minimax(o: BackendProviderOptions): LLMProvider {
  return build("minimax", o)
}

/** Google Gemini (single wire). */
export function gemini(o: Omit<BackendProviderOptions, "protocol">): LLMProvider {
  const model = o.model ?? defaultModelForProvider("gemini")
  return PROVIDER_REGISTRY["gemini:gemini"](
    o.apiKey,
    model,
    o.retry,
    o.baseURL,
    getRuntimePolicy("gemini", model),
  )
}

/** Local Ollama (single wire, no API key). */
export function ollama(o: { model?: string; baseURL?: string } = {}): LLMProvider {
  const model = o.model ?? defaultModelForProvider("ollama")
  return new OllamaProvider(model, o.baseURL, getRuntimePolicy("ollama", model))
}
