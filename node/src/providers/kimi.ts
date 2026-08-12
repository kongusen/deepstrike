import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { openAIChatDialects } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/** @deprecated Prefer `kimi({ protocol: "anthropic" })`. */
export class KimiAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string, runtimePolicy?: RuntimePolicy) {
    super(anthropicVendorProfiles.kimi, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

/** @deprecated Prefer the `kimi()` factory. Runtime behavior is dialect data. */
export class KimiProvider extends OpenAIChatProvider {
  constructor(apiKey: string, model = "kimi-k2.6", retry?: { maxRetries: number; baseDelay: number }, baseURL: string = endpointProfiles["kimi.openai"].baseURL, runtimePolicy: RuntimePolicy = {}) {
    super(apiKey, model, retry, baseURL, runtimePolicy, openAIChatDialects.kimi)
  }
}
