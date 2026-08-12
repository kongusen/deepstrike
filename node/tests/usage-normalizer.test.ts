/**
 * spc_011-C-07: `UsageNormalizer` — real/representative raw usage samples per wire family,
 * normalized to `ProviderUsage`. Samples are shaped exactly like each SDK's real
 * usage type (verified against `node_modules/openai`, `node_modules/@anthropic-ai/sdk`,
 * `node_modules/@google/generative-ai` — see `usage-normalizer.ts`'s own doc comments for the
 * field-name citations), not guessed.
 */
import {
  normalizeOpenAIUsage,
  normalizeAnthropicUsage,
  normalizeGeminiUsage,
  normalizeOllamaUsage,
} from "../src/providers/usage-normalizer.js"

describe("UsageNormalizer — OpenAI wire family (Chat Completions + Responses)", () => {
  it("normalizes a Chat Completions usage object, reusing openAICachedPromptTokens for the cache figure", () => {
    const usage = {
      prompt_tokens: 1000,
      completion_tokens: 250,
      total_tokens: 1250,
      prompt_tokens_details: { cached_tokens: 400 },
      completion_tokens_details: { reasoning_tokens: 80 },
    }
    expect(normalizeOpenAIUsage(usage)).toEqual({
      inputTokens: 1000,
      outputTokens: 250,
      cacheReadInputTokens: 400,
      reasoningTokens: 80,
    })
  })

  it("normalizes a Responses API usage object (input_tokens/output_tokens field names)", () => {
    const usage = {
      input_tokens: 500,
      output_tokens: 120,
      total_tokens: 620,
      input_tokens_details: { cached_tokens: 0 },
      output_tokens_details: { reasoning_tokens: 40 },
    }
    expect(normalizeOpenAIUsage(usage)).toEqual({ inputTokens: 500, outputTokens: 120, reasoningTokens: 40 })
  })

  it("normalizes DeepSeek's prompt_cache_hit_tokens variant via the shared helper", () => {
    const usage = { prompt_tokens: 800, completion_tokens: 100, prompt_cache_hit_tokens: 300 }
    expect(normalizeOpenAIUsage(usage)?.cacheReadInputTokens).toBe(300)
  })

  it("handles a missing/malformed usage object without throwing", () => {
    expect(normalizeOpenAIUsage(undefined)).toBeUndefined()
  })
})

describe("UsageNormalizer — Anthropic wire family", () => {
  it("sums uncached + cache_read + cache_creation into the full prompt size (matches anthropic.ts's own stream() formula)", () => {
    const usage = {
      input_tokens: 200,
      output_tokens: 150,
      cache_read_input_tokens: 500,
      cache_creation_input_tokens: 100,
    }
    expect(normalizeAnthropicUsage(usage)).toEqual({
      inputTokens: 800, // 200 + 500 + 100
      outputTokens: 150,
      cacheReadInputTokens: 500,
      cacheCreationInputTokens: 100,
    })
  })

  it("has no reasoning-token field to report", () => {
    expect(normalizeAnthropicUsage({ input_tokens: 10, output_tokens: 5 })?.reasoningTokens).toBeUndefined()
  })
})

describe("UsageNormalizer — Gemini", () => {
  it("normalizes promptTokenCount/candidatesTokenCount/cachedContentTokenCount", () => {
    const usage = { promptTokenCount: 900, candidatesTokenCount: 60, totalTokenCount: 960, cachedContentTokenCount: 200 }
    expect(normalizeGeminiUsage(usage)).toEqual({ inputTokens: 900, outputTokens: 60, cacheReadInputTokens: 200 })
  })
})

describe("UsageNormalizer — Ollama (first-ever usage extraction)", () => {
  it("normalizes prompt_eval_count/eval_count with no cache or reasoning concept", () => {
    const chunk = { done: true, prompt_eval_count: 300, eval_count: 45 }
    expect(normalizeOllamaUsage(chunk)).toEqual({ inputTokens: 300, outputTokens: 45 })
  })
})
