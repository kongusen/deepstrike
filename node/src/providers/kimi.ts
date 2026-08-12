import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { endpointProfiles } from "./endpoints.js"
import { anthropicVendorProfiles } from "./vendor-profiles.js"

/**
 * Kimi over its Anthropic-compatible endpoint.
 * @deprecated Prefer `kimi({ protocol: "anthropic" })`. Behavior is now fully
 * data-driven via `anthropicVendorProfiles.kimi`; this thin shim is kept for
 * backward compatibility and `instanceof` checks.
 */
export class KimiAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
    runtimePolicy?: RuntimePolicy,
  ) {
    super(anthropicVendorProfiles.kimi, apiKey, model, retry, baseURL, runtimePolicy)
  }
}

export class KimiProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "kimi-k2.6",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["kimi.openai"].baseURL,
    runtimePolicy: RuntimePolicy = {},
  ) {
    super(apiKey, model, retry, baseURL, runtimePolicy)
  }

  override runtimePolicy(): RuntimePolicy {
    return super.runtimePolicy()
  }

  override descriptor(): ProviderDescriptor {
    return {
      ...super.descriptor(),
      provider: "kimi",
      model: this.model,
    }
  }
}
