/**
 * spc_011-C-07: normalizes each vendor's raw postflight usage object into the shared
 * `ProviderUsage` shape (`../types.js`). One function per wire family, not one
 * per provider — DeepSeek/Kimi/GLM/Qwen/MiniMax's OpenAI-wire variants all route through
 * `normalizeOpenAIUsage` via `OpenAIChatProvider` inheritance, and their Anthropic-wire variants
 * route through `normalizeAnthropicUsage` via `AnthropicCompatibleProvider` inheritance — the
 * "8 providers" the spec counts are 8 *providers*, not 8 independent parsing implementations.
 *
 * Reuses `openAICachedPromptTokens` (`./base.js`) for the OpenAI-family cache figure rather than
 * re-deriving it — that function already covers the OpenAI/Qwen/MiniMax/GLM/Kimi standard shape
 * plus DeepSeek's `prompt_cache_hit_tokens` variant.
 *
 * `TokenUsage`/`UsageEvent` are otherwise untouched by this module. Missing or malformed usage
 * returns `undefined`; absence is not a zero-token measurement.
 */
import type { ProviderUsage } from "../types.js"
import { openAICachedPromptTokens } from "./base.js"

function readNumber(obj: Record<string, unknown> | undefined, key: string): number | undefined {
  const value = obj?.[key]
  return typeof value === "number" ? value : undefined
}

/**
 * Covers both OpenAI wire shapes actually in use: Chat Completions (`prompt_tokens`/
 * `completion_tokens`/`completion_tokens_details.reasoning_tokens`, used by `openai.ts` and every
 * `OpenAIChatProvider` subclass) and the Responses API (`input_tokens`/`output_tokens`/
 * `output_tokens_details.reasoning_tokens`, used by `openai-responses.ts`) — the two field-name
 * conventions are read via fallback, and only one will ever be present on a given raw object.
 */
export function normalizeOpenAIUsage(usage: unknown): ProviderUsage | undefined {
  const u = usage && typeof usage === "object" ? (usage as Record<string, unknown>) : undefined
  const rawInput = readNumber(u, "prompt_tokens") ?? readNumber(u, "input_tokens")
  const rawOutput = readNumber(u, "completion_tokens") ?? readNumber(u, "output_tokens")
  if (rawInput === undefined && rawOutput === undefined) return undefined
  const inputTokens = rawInput ?? 0
  const outputTokens = rawOutput ?? 0
  const cacheReadInputTokens = openAICachedPromptTokens(usage)
  const details = (u?.completion_tokens_details ?? u?.output_tokens_details) as Record<string, unknown> | undefined
  const reasoningTokens = readNumber(details, "reasoning_tokens")
  const providerUsage: ProviderUsage = {
    inputTokens,
    outputTokens,
    ...(cacheReadInputTokens > 0 ? { cacheReadInputTokens } : {}),
    ...(reasoningTokens !== undefined ? { reasoningTokens } : {}),
  }
  return providerUsage
}

/**
 * Anthropic's raw `input_tokens` is UNCACHED input only (unlike OpenAI's `prompt_tokens`, which
 * is already cache-inclusive) — `anthropic.ts`'s own `stream()` sums `input_tokens +
 * cache_read_input_tokens + cache_creation_input_tokens` into the full prompt size for the same
 * reason context-pressure accounting needs it (see that file's comment on why excluding cached
 * tokens would suppress compaction until a 413). This function replicates that same sum so
 * `ProviderUsage.inputTokens` means the same "full prompt size" thing across vendors — reading
 * raw `input_tokens` alone here would silently undercount every cache-heavy Anthropic turn.
 * No `reasoning_tokens`-equivalent field exists in this SDK's `Usage` type, so that field stays
 * unset rather than guessed.
 */
export function normalizeAnthropicUsage(usage: unknown): ProviderUsage | undefined {
  const u = usage && typeof usage === "object" ? (usage as Record<string, unknown>) : undefined
  const rawUncachedInput = readNumber(u, "input_tokens")
  const rawCacheRead = readNumber(u, "cache_read_input_tokens")
  const rawCacheCreation = readNumber(u, "cache_creation_input_tokens")
  const rawOutput = readNumber(u, "output_tokens")
  if (rawUncachedInput === undefined && rawCacheRead === undefined && rawCacheCreation === undefined && rawOutput === undefined) return undefined
  const uncachedInput = rawUncachedInput ?? 0
  const cacheReadInputTokens = rawCacheRead ?? 0
  const cacheCreationInputTokens = rawCacheCreation ?? 0
  const outputTokens = rawOutput ?? 0
  const providerUsage: ProviderUsage = {
    inputTokens: uncachedInput + cacheReadInputTokens + cacheCreationInputTokens,
    outputTokens,
    ...(cacheReadInputTokens > 0 ? { cacheReadInputTokens } : {}),
    ...(cacheCreationInputTokens > 0 ? { cacheCreationInputTokens } : {}),
  }
  return providerUsage
}

/**
 * `@google/generative-ai`'s `UsageMetadata` has no reasoning/thoughts token field and no separate
 * cache-write count (only `cachedContentTokenCount`, a read figure) — `reasoningTokens` and
 * `cacheCreationInputTokens` stay unset rather than invented.
 */
export function normalizeGeminiUsage(usage: unknown): ProviderUsage | undefined {
  const u = usage && typeof usage === "object" ? (usage as Record<string, unknown>) : undefined
  const rawInput = readNumber(u, "promptTokenCount")
  const rawOutput = readNumber(u, "candidatesTokenCount")
  if (rawInput === undefined && rawOutput === undefined) return undefined
  const inputTokens = rawInput ?? 0
  const outputTokens = rawOutput ?? 0
  const cacheReadInputTokens = readNumber(u, "cachedContentTokenCount")
  const providerUsage: ProviderUsage = {
    inputTokens,
    outputTokens,
    ...(cacheReadInputTokens ? { cacheReadInputTokens } : {}),
  }
  return providerUsage
}

/**
 * Ollama's final stream chunk (`done: true`) carries `prompt_eval_count`/`eval_count` — no cache
 * or reasoning concept exists in its API at all, so both stay unset. Before this card `ollama.ts`
 * read neither field; this is the first usage extraction it has ever had.
 */
export function normalizeOllamaUsage(chunk: unknown): ProviderUsage | undefined {
  const u = chunk && typeof chunk === "object" ? (chunk as Record<string, unknown>) : undefined
  const rawInput = readNumber(u, "prompt_eval_count")
  const rawOutput = readNumber(u, "eval_count")
  if (rawInput === undefined && rawOutput === undefined) return undefined
  return { inputTokens: rawInput ?? 0, outputTokens: rawOutput ?? 0 }
}
