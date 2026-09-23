/** Provider request builders that encode canonical host input into vendor request plans. */

import type { BoundaryProtocol } from "./types.js"

export const PROVIDER_REQUEST_BUILD_PROTOCOL: BoundaryProtocol = {
  id: "provider-request.build",
  family: "host-provider",
  direction: "encode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "providers/anthropic-adapter:AnthropicMessagesAdapter.buildRequest",
      source: { type: "CanonicalAdapterInput", layer: "host", authority: "host-runtime" },
      target: { type: "AnthropicRequestPlan", layer: "provider", authority: "provider" },
      fields: { forbidden: [] },
    },
    {
      adapter: "providers/gemini-adapter:GeminiAdapter.buildRequest",
      source: { type: "CanonicalAdapterInput", layer: "host", authority: "host-runtime" },
      target: { type: "GeminiRequestPlan", layer: "provider", authority: "provider" },
      fields: { forbidden: [] },
    },
    {
      adapter: "providers/ollama-adapter:OllamaAdapter.buildRequest",
      source: { type: "CanonicalAdapterInput", layer: "host", authority: "host-runtime" },
      target: { type: "OllamaRequest", layer: "provider", authority: "provider" },
      fields: { forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Provider semantic tests cover protocol-specific request plans and wire extensions." },
}
