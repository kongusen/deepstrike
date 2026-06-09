import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"

const GLM_BASE = endpointProfiles["glm.openai"].baseURL

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

export class GLMProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "glm-5.1",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = GLM_BASE,
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
