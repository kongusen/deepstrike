/** Provider wire usage decoders used by the live provider adapters. */

import type { BoundaryProtocol } from "./types.js"

export const PROVIDER_USAGE_DECODE_PROTOCOL: BoundaryProtocol = {
  id: "provider-usage.decode",
  family: "host-provider",
  direction: "decode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "providers/usage-normalizer:normalizeOpenAIUsage",
      source: { type: "unknown", layer: "provider", authority: "provider" },
      target: { type: "ProviderUsage | undefined", layer: "host", authority: "host-runtime" },
      fields: { forbidden: [] },
    },
    {
      adapter: "providers/usage-normalizer:normalizeAnthropicUsage",
      source: { type: "unknown", layer: "provider", authority: "provider" },
      target: { type: "ProviderUsage | undefined", layer: "host", authority: "host-runtime" },
      fields: { forbidden: [] },
    },
    {
      adapter: "providers/usage-normalizer:normalizeGeminiUsage",
      source: { type: "unknown", layer: "provider", authority: "provider" },
      target: { type: "ProviderUsage | undefined", layer: "host", authority: "host-runtime" },
      fields: { forbidden: [] },
    },
    {
      adapter: "providers/usage-normalizer:normalizeOllamaUsage",
      source: { type: "unknown", layer: "provider", authority: "provider" },
      target: { type: "ProviderUsage | undefined", layer: "host", authority: "host-runtime" },
      fields: { forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Usage tests cover provider-specific fields, invalid values, and missing usage." },
}
