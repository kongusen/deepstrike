// Single source of truth for the Anthropic-compatible vendor backends (DeepSeek,
// Kimi, Qwen, GLM, MiniMax). Each backend differs only by data — provider id,
// default model, endpoint, and per-model runtime policy — so the generic
// `AnthropicCompatibleProvider` reads a profile from here instead of every
// backend subclassing `AnthropicProvider` purely to carry configuration.
//
// Runtime policy and default model resolution live in ModelRegistry; this module
// only carries Anthropic-compatible transport configuration.
import { endpointProfiles } from "./endpoints.js"
import type { ProviderId } from "./endpoints.js"

export type EndpointProfileKey = keyof typeof endpointProfiles

export interface AnthropicVendorProfile {
  /** Identity advertised in `descriptor().provider`. */
  providerId: ProviderId
  /** Model used when the caller does not pass one. */
  defaultModel: string
  /** Endpoint profile whose `baseURL` is the Anthropic-compatible wire for this vendor. */
  baseURLProfileKey: EndpointProfileKey
}

export const anthropicVendorProfiles = {
  deepseek: { providerId: "deepseek", defaultModel: "deepseek-v4-flash", baseURLProfileKey: "deepseek.anthropic" },
  kimi:     { providerId: "kimi",     defaultModel: "kimi-k2.6",         baseURLProfileKey: "kimi.anthropic" },
  qwen:     { providerId: "qwen",     defaultModel: "qwen3.6-plus",      baseURLProfileKey: "qwen.anthropic" },
  glm:      { providerId: "glm",      defaultModel: "glm-5.2",           baseURLProfileKey: "glm.anthropic" },
  minimax:  { providerId: "minimax",  defaultModel: "MiniMax-M3",        baseURLProfileKey: "minimax.anthropic" },
} satisfies Record<string, AnthropicVendorProfile>

export type AnthropicVendorId = keyof typeof anthropicVendorProfiles

/** Resolve the Anthropic-compatible base URL for a vendor profile. */
export function anthropicVendorBaseURL(profile: AnthropicVendorProfile): string {
  return endpointProfiles[profile.baseURLProfileKey].baseURL
}
