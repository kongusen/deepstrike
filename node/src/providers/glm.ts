import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { endpointProfiles } from "./profiles.js"
import { GLM_POLICIES, anthropicVendorProfiles } from "./vendor-profiles.js"

/**
 * GLM over its Anthropic-compatible endpoint.
 * @deprecated Prefer `glm({ protocol: "anthropic" })`. Behavior is now fully
 * data-driven via `anthropicVendorProfiles.glm`; this thin shim is kept for
 * backward compatibility and `instanceof` checks.
 */
export class GLMAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
  ) {
    super(anthropicVendorProfiles.glm, apiKey, model, retry, baseURL)
  }
}

export class GLMProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "glm-5.1",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["glm.openai"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return GLM_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      ...super.descriptor(),
      provider: "glm",
      model: this.model,
    }
  }
}
