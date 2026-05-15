import { AnthropicProvider } from "./anthropic.js"
import { endpointProfiles } from "./profiles.js"

export class MiniMaxProvider extends AnthropicProvider {
  constructor(
    apiKey: string,
    model: "MiniMax-M2.7" | "MiniMax-M2.5" = "MiniMax-M2.7",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["minimax.anthropic"].baseURL,
  ) {
    super(apiKey, model, retry, {
      baseURL,
      authMode: "bearer",
    })
  }
}
