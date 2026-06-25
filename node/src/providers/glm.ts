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
    model: string = "glm-5.2",
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

  // ── GLM web_search (Zhipu vendor server tool; OpenAI-wire only) ──────────────
  // Enable with `extensions={ web_search: true }` (default config) or `{ web_search: {...} }`
  // (passthrough: search_engine, search_recency_filter, search_domain_filter, count, …). Injected as a
  // `{ type: "web_search", web_search: {...} }` entry in tools[]; the model searches server-side and
  // the results come back inline (no client tool-loop). Mirrors the Python GLM provider.
  protected override serverTools(extensions?: Record<string, unknown>): unknown[] {
    const ws = extensions?.web_search
    if (!ws) return []
    return [{ type: "web_search", web_search: typeof ws === "object" ? ws : {} }]
  }

  // Strip `web_search` from the passthrough so it shapes tools[] only, never leaks as a body field.
  protected override prepareExtensions(extensions?: Record<string, unknown>): Record<string, unknown> | undefined {
    if (!extensions || !("web_search" in extensions)) return extensions
    const { web_search: _omit, ...rest } = extensions
    return rest
  }
}
