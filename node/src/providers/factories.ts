// Per-backend provider factories — one function per backend, replacing the dual
// `<Backend>Provider` / `<Backend>AnthropicProvider` class families in the public API. Where a backend
// offers both an OpenAI- and an Anthropic-compatible wire, the `protocol` option selects it (the two
// have genuinely different request/replay logic, so they remain distinct internal classes). For OpenAI
// itself use the root `OpenAIProvider` / `OpenAIResponsesProvider`; for any model by id use `createProvider`.
import type { LLMProvider } from "../types.js"
import { DeepSeekProvider, DeepSeekAnthropicProvider } from "./deepseek.js"
import { KimiProvider, KimiAnthropicProvider } from "./kimi.js"
import { QwenProvider, QwenAnthropicProvider } from "./qwen.js"
import { GLMProvider, GLMAnthropicProvider } from "./glm.js"
import { MiniMaxOpenAIProvider, MiniMaxAnthropicProvider } from "./minimax.js"
import { GeminiProvider } from "./gemini.js"
import { OllamaProvider } from "./ollama.js"

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

/** DeepSeek. Defaults to the OpenAI-compatible wire (richer reasoning-replay handling). */
export function deepseek(o: BackendProviderOptions): LLMProvider {
  return o.protocol === "anthropic"
    ? new DeepSeekAnthropicProvider(o.apiKey, o.model, o.retry, o.baseURL)
    : new DeepSeekProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** Moonshot Kimi. Defaults to the OpenAI-compatible wire. */
export function kimi(o: BackendProviderOptions): LLMProvider {
  return o.protocol === "anthropic"
    ? new KimiAnthropicProvider(o.apiKey, o.model, o.retry, o.baseURL)
    : new KimiProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** Alibaba Qwen / DashScope. Defaults to the OpenAI-compatible (DashScope) wire. */
export function qwen(o: BackendProviderOptions): LLMProvider {
  return o.protocol === "anthropic"
    ? new QwenAnthropicProvider(o.apiKey, o.model, o.retry, o.baseURL)
    : new QwenProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** Zhipu GLM. Defaults to the OpenAI-compatible wire. */
export function glm(o: BackendProviderOptions): LLMProvider {
  return o.protocol === "anthropic"
    ? new GLMAnthropicProvider(o.apiKey, o.model, o.retry, o.baseURL)
    : new GLMProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** MiniMax. Defaults to the Anthropic-compatible wire (the primary M2.x path). */
export function minimax(o: BackendProviderOptions): LLMProvider {
  return o.protocol === "openai"
    ? new MiniMaxOpenAIProvider(o.apiKey, o.model, o.retry, o.baseURL)
    : new MiniMaxAnthropicProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** Google Gemini (single wire). */
export function gemini(o: Omit<BackendProviderOptions, "protocol">): LLMProvider {
  return new GeminiProvider(o.apiKey, o.model, o.retry, o.baseURL)
}

/** Local Ollama (single wire, no API key). */
export function ollama(o: { model?: string; baseURL?: string } = {}): LLMProvider {
  return new OllamaProvider(o.model, o.baseURL)
}
