import type { RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { endpointProfiles } from "./profiles.js"

const MINIMAX_POLICIES: Record<string, RuntimePolicy> = {
  "MiniMax-M2.7":    { maxTurns: 35 },
  "MiniMax-M2.5":    { maxTurns: 25 },
  "MiniMax-Text-01": { maxTurns: 20 },
}

export class MiniMaxProvider extends AnthropicProvider {
  constructor(
    apiKey: string,
    model: string = "MiniMax-M2.7",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["minimax.anthropic"].baseURL,
  ) {
    super(apiKey, model, retry, {
      baseURL,
      authMode: "api-key",
    })
  }

  override runtimePolicy(): RuntimePolicy {
    return MINIMAX_POLICIES[this.model] ?? {}
  }
}
