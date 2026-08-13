import type { RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { anthropicVendorBaseURL, type AnthropicVendorProfile } from "./vendor-profiles.js"

/**
 * A vendor that exposes an Anthropic-compatible Messages endpoint (DeepSeek,
 * Kimi, Qwen, GLM, MiniMax, …). All wire behavior is inherited from
 * `AnthropicProvider`; the only per-vendor variation is configuration, supplied
 * as an `AnthropicVendorProfile`. This replaces the family of near-identical
 * `<Vendor>AnthropicProvider` subclasses that existed only to carry that config.
 *
 * Adding a new Anthropic-compatible vendor is now "add a profile" — no new
 * provider class is required.
 */
export class AnthropicCompatibleProvider extends AnthropicProvider {
  private readonly vendorProfile: AnthropicVendorProfile

  constructor(
    profile: AnthropicVendorProfile,
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
    runtimePolicy?: RuntimePolicy,
  ) {
    super({
      apiKey,
      model: model ?? profile.defaultModel,
      retry,
      baseURL: baseURL ?? anthropicVendorBaseURL(profile),
      authMode: "api-key",
      runtimePolicy,
    })
    this.vendorProfile = profile
  }

  protected override providerName(): string {
    return this.vendorProfile.providerId
  }

  override runtimePolicy(): RuntimePolicy {
    return super.runtimePolicy()
  }
}
