import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { openAIChatDialects } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/** @deprecated Prefer `minimax({ protocol: "anthropic" })`. */
export class MiniMaxAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string, runtimePolicy?: RuntimePolicy) {
    super(anthropicVendorProfiles.minimax, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

/** @deprecated Prefer the `minimax({ protocol: "openai" })` factory. Runtime behavior is dialect data. */
export class MiniMaxOpenAIProvider extends OpenAIChatProvider {
  constructor(apiKey: string, model = "MiniMax-M3", retry?: { maxRetries: number; baseDelay: number }, baseURL: string = endpointProfiles["minimax.openai"].baseURL, runtimePolicy: RuntimePolicy = {}) {
    super(apiKey, model, retry, baseURL, runtimePolicy, openAIChatDialects.minimax)
  }
}
