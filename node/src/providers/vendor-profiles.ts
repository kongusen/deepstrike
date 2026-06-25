// Single source of truth for the Anthropic-compatible vendor backends (DeepSeek,
// Kimi, Qwen, GLM, MiniMax). Each backend differs only by data — provider id,
// default model, endpoint, and per-model runtime policy — so the generic
// `AnthropicCompatibleProvider` reads a profile from here instead of every
// backend subclassing `AnthropicProvider` purely to carry configuration.
//
// The per-model policy tables are also consumed by each backend's OpenAI-chat
// class (which shares the same recommended maxTurns), so they are exported here
// to keep one authoritative copy. Imports nothing from the backend files, so
// there is no import cycle.
import type { RuntimePolicy } from "../types.js"
import { endpointProfiles } from "./profiles.js"
import type { ProviderId } from "./profiles.js"

export type EndpointProfileKey = keyof typeof endpointProfiles

export interface AnthropicVendorProfile {
  /** Identity advertised in `descriptor().provider`. */
  providerId: ProviderId
  /** Model used when the caller does not pass one. */
  defaultModel: string
  /** Endpoint profile whose `baseURL` is the Anthropic-compatible wire for this vendor. */
  baseURLProfileKey: EndpointProfileKey
  /** Recommended `maxTurns` per model id; missing model → empty policy. */
  policies: Record<string, RuntimePolicy>
}

export const DEEPSEEK_POLICIES: Record<string, RuntimePolicy> = {
  "deepseek-chat":      { maxTurns: 25 },
  "deepseek-reasoner":  { maxTurns: 50 },
  "deepseek-v4-flash":  { maxTurns: 20 },
  "deepseek-v4-pro":    { maxTurns: 35 },
}

export const KIMI_POLICIES: Record<string, RuntimePolicy> = {
  "moonshot-v1-8k":   { maxTurns: 15 },
  "moonshot-v1-32k":  { maxTurns: 20 },
  "moonshot-v1-128k": { maxTurns: 30 },
  "kimi-k2.5":        { maxTurns: 30 },
  "kimi-k2.6":        { maxTurns: 35 },
  "kimi-k2-thinking": { maxTurns: 50 },
  "kimi-k2-thinking-turbo": { maxTurns: 40 },
}

export const QWEN_POLICIES: Record<string, RuntimePolicy> = {
  "qwen3.7-max-preview": { maxTurns: 45 },
  "qwen3.7-plus-preview": { maxTurns: 40 },
  "qwen3.6-max-preview": { maxTurns: 40 },
  "qwen3.6-plus": { maxTurns: 35 },
  "qwen3.6-flash": { maxTurns: 20 },
  "qwen3.6-35b-a3b": { maxTurns: 25 },
  "qwen3.6-27b": { maxTurns: 25 },
  "qwen3.5-plus": { maxTurns: 35 },
  "qwen3.5-flash": { maxTurns: 20 },
  "qwen3.5-397b-a17b": { maxTurns: 35 },
  "qwen3.5-122b-a10b": { maxTurns: 25 },
  "qwen3.5-35b-a3b": { maxTurns: 20 },
  "qwen3.5-27b": { maxTurns: 20 },
}

export const GLM_POLICIES: Record<string, RuntimePolicy> = {
  "glm-5.2": { maxTurns: 50 },
  "glm/glm-5.2": { maxTurns: 50 },
  "glm-5.1": { maxTurns: 50 },
  "glm/glm-5.1": { maxTurns: 50 },
  "glm-4-plus": { maxTurns: 35 },
  "glm/glm-4-plus": { maxTurns: 35 },
  "glm-4-flash": { maxTurns: 15 },
  "glm/glm-4-flash": { maxTurns: 15 },
  "glm-4-air": { maxTurns: 20 },
  "glm/glm-4-air": { maxTurns: 20 },
}

export const MINIMAX_POLICIES: Record<string, RuntimePolicy> = {
  "MiniMax-M3":             { maxTurns: 35 },
  "MiniMax-M3-highspeed":   { maxTurns: 35 },
  "MiniMax-M2.7":           { maxTurns: 35 },
  "MiniMax-M2.7-highspeed": { maxTurns: 35 },
  "MiniMax-M2.5":           { maxTurns: 25 },
  "MiniMax-M2.5-highspeed": { maxTurns: 25 },
  "MiniMax-M2.1":           { maxTurns: 25 },
  "MiniMax-M2.1-highspeed": { maxTurns: 25 },
  "MiniMax-M2":             { maxTurns: 20 },
  "MiniMax-Text-01":        { maxTurns: 20 },
}

export const anthropicVendorProfiles = {
  deepseek: { providerId: "deepseek", defaultModel: "deepseek-v4-flash", baseURLProfileKey: "deepseek.anthropic", policies: DEEPSEEK_POLICIES },
  kimi:     { providerId: "kimi",     defaultModel: "kimi-k2.6",         baseURLProfileKey: "kimi.anthropic",     policies: KIMI_POLICIES },
  qwen:     { providerId: "qwen",     defaultModel: "qwen3.6-plus",      baseURLProfileKey: "qwen.anthropic",     policies: QWEN_POLICIES },
  glm:      { providerId: "glm",      defaultModel: "glm-5.2",           baseURLProfileKey: "glm.anthropic",      policies: GLM_POLICIES },
  minimax:  { providerId: "minimax",  defaultModel: "MiniMax-M3",        baseURLProfileKey: "minimax.anthropic",  policies: MINIMAX_POLICIES },
} satisfies Record<string, AnthropicVendorProfile>

export type AnthropicVendorId = keyof typeof anthropicVendorProfiles

/** Resolve the Anthropic-compatible base URL for a vendor profile. */
export function anthropicVendorBaseURL(profile: AnthropicVendorProfile): string {
  return endpointProfiles[profile.baseURLProfileKey].baseURL
}
