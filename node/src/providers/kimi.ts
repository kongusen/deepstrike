import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { endpointProfiles } from "./profiles.js"
import { KIMI_POLICIES, anthropicVendorProfiles } from "./vendor-profiles.js"

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
  ) {
    super(anthropicVendorProfiles.kimi, apiKey, model, retry, baseURL)
  }
}

export class KimiProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "kimi-k2.6",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["kimi.openai"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return KIMI_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      ...super.descriptor(),
      provider: "kimi",
      model: this.model,
    }
  }
}
