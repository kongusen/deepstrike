import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"

const MOONSHOT_BASE = endpointProfiles["kimi.openai"].baseURL

export class KimiProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: "kimi-k2.5" | "kimi-k2.6" = "kimi-k2.6",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = MOONSHOT_BASE,
  ) {
    super(apiKey, model, retry, baseURL)
  }
}
