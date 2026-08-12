import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { openAIChatDialects } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/** @deprecated Prefer `deepseek({ protocol: "anthropic" })`. */
export class DeepSeekAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string, runtimePolicy?: RuntimePolicy) {
    super(anthropicVendorProfiles.deepseek, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

/** @deprecated Prefer the `deepseek()` factory. Runtime behavior is dialect data. */
export class DeepSeekProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model = "deepseek-v4-flash",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["deepseek.openai"].baseURL,
    runtimePolicy: RuntimePolicy = {},
  ) {
    super(apiKey, model, retry, baseURL, runtimePolicy, openAIChatDialects.deepseek)
  }
}
