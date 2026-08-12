import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { openAIChatDialects } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/** @deprecated Prefer `glm({ protocol: "anthropic" })`. */
export class GLMAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string, runtimePolicy?: RuntimePolicy) {
    super(anthropicVendorProfiles.glm, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

/** @deprecated Prefer the `glm()` factory. Runtime behavior is dialect data. */
export class GLMProvider extends OpenAIChatProvider {
  constructor(apiKey: string, model = "glm-5.2", retry?: { maxRetries: number; baseDelay: number }, baseURL: string = endpointProfiles["glm.openai"].baseURL, runtimePolicy: RuntimePolicy = {}) {
    super(apiKey, model, retry, baseURL, runtimePolicy, openAIChatDialects.glm)
  }
}
