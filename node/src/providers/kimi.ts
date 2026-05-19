import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"

const MOONSHOT_BASE = endpointProfiles["kimi.openai"].baseURL

const KIMI_POLICIES: Record<string, RuntimePolicy> = {
  "moonshot-v1-8k":   { maxTurns: 15 },
  "moonshot-v1-32k":  { maxTurns: 20 },
  "moonshot-v1-128k": { maxTurns: 30 },
  "kimi-k2.5":        { maxTurns: 30 },
  "kimi-k2.6":        { maxTurns: 35 },
}

export class KimiProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "kimi-k2.6",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = MOONSHOT_BASE,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return KIMI_POLICIES[this.model] ?? {}
  }
}
