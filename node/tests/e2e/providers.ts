/**
 * Create real LLM providers from environment variables.
 * Returns null for any provider whose API key is missing (scenario will be skipped).
 */
import { OpenAIChatProvider } from "../../src/providers/openai.js"
import { deepseek, minimax } from "../../src/providers/factories.js"
import type { LLMProvider } from "../../src/types.js"

export type ProviderName = "openai" | "deepseek" | "minimax"

export interface ProviderSlot {
  name: ProviderName
  provider: LLMProvider | null
  skipReason?: string
}

export function loadProviders(): Record<ProviderName, LLMProvider | null> {
  const openaiKey = process.env.OPENAI_API_KEY
  const deepseekKey = process.env.DEEPSEEK_API_KEY
  const minimaxKey = process.env.MINIMAX_API_KEY

  return {
    openai: openaiKey
      ? new OpenAIChatProvider({
          apiKey: openaiKey,
          model: process.env.OPENAI_MODEL ?? "gpt-4o-mini",
          retry: { maxRetries: 2, baseDelay: 1000 },
          baseURL: process.env.OPENAI_BASE_URL,
        })
      : null,
    deepseek: deepseekKey
      ? deepseek({ apiKey: deepseekKey, model: process.env.DEEPSEEK_MODEL ?? "deepseek-chat" })
      : null,
    minimax: minimaxKey
      ? minimax({ apiKey: minimaxKey, model: process.env.MINIMAX_MODEL })
      : null,
  }
}

/** Pick the requested provider, or MiniMax first for live stress coverage. */
export function anyProvider(providers: Record<ProviderName, LLMProvider | null>): LLMProvider | null {
  const requested = process.env.E2E_PROVIDER?.trim().toLowerCase() as ProviderName | undefined
  if (requested) return providers[requested] ?? null
  return providers.minimax ?? providers.openai ?? providers.deepseek ?? null
}
