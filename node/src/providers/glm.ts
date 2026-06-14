import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"

const GLM_POLICIES: Record<string, RuntimePolicy> = {
  "glm-5.1": { maxTurns: 50 },
  "glm/glm-5.1": { maxTurns: 50 },
  "glm-4-plus": { maxTurns: 35 },
  "glm/glm-4-plus": { maxTurns: 35 },
  "glm-4-flash": { maxTurns: 15 },
  "glm/glm-4-flash": { maxTurns: 15 },
  "glm-4-air": { maxTurns: 20 },
  "glm/glm-4-air": { maxTurns: 20 },
}

/**
 * GLM over its Anthropic-compatible endpoint.
 */
export class GLMAnthropicProvider extends AnthropicProvider {
  constructor(
    apiKey: string,
    model: string = "glm-5.1",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["glm.anthropic"].baseURL,
  ) {
    super(apiKey, model, retry, {
      baseURL,
      authMode: "api-key",
    })
  }

  protected override providerName(): string {
    return "glm"
  }

  override runtimePolicy(): RuntimePolicy {
    return GLM_POLICIES[this.model] ?? {}
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
