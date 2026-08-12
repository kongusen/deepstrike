import type { RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { openAIChatDialects } from "./openai-chat-dialects.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/** @deprecated Prefer `qwen({ protocol: "anthropic" })`. */
export class QwenAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(apiKey: string, model?: string, retry?: { maxRetries: number; baseDelay: number }, baseURL?: string, runtimePolicy?: RuntimePolicy) {
    super(anthropicVendorProfiles.qwen, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

/** @deprecated Prefer the `qwen()` factory. Runtime behavior is dialect data. */
export class QwenProvider extends OpenAIChatProvider {
  constructor(apiKey: string, model = "qwen3.6-plus", retry?: { maxRetries: number; baseDelay: number }, baseURL: string = endpointProfiles["qwen.dashscope"].baseURL, runtimePolicy: RuntimePolicy = {}) {
    super(apiKey, model, retry, baseURL, runtimePolicy, openAIChatDialects.qwen)
  }
}
