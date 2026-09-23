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
      fields: {
        nested: [
          { source: "context", target: "params", kind: "project", note: "system and turn messages become Anthropic params." },
          { source: "tools", target: "params", kind: "project", note: "canonical tools become provider tool blocks." },
          { source: "resolved", target: "params", kind: "project", note: "resolved model identity selects the wire model." },
          { source: "extensions", target: "params", kind: "project", note: "provider extensions are filtered into wire params." },
        ],
        forbidden: [],
      },
    },
    {
      adapter: "providers/gemini-adapter:GeminiAdapter.buildRequest",
      source: { type: "CanonicalAdapterInput", layer: "host", authority: "host-runtime" },
      target: { type: "GeminiRequestPlan", layer: "provider", authority: "provider" },
      fields: {
        nested: [
          { source: "context", target: "request", kind: "project", note: "canonical turns become Gemini contents." },
          { source: "tools", target: "request", kind: "project", note: "canonical tools become function declarations." },
          { source: "resolved", target: "modelParams", kind: "project", note: "resolved model identity configures generation." },
          { source: "extensions", target: "modelParams", kind: "project", note: "provider extensions configure generation." },
        ],
        forbidden: [],
      },
    },
    {
      adapter: "providers/ollama-adapter:OllamaAdapter.buildRequest",
      source: { type: "CanonicalAdapterInput", layer: "host", authority: "host-runtime" },
      target: { type: "OllamaRequest", layer: "provider", authority: "provider" },
      fields: {
        nested: [
          { source: "context", target: "messages", kind: "project", note: "system and turns become Ollama messages." },
          { source: "tools", target: "tools", kind: "project", note: "canonical tools become Ollama function tools." },
          { source: "resolved", target: "model", kind: "project", note: "resolved model identity selects the wire model." },
          { source: "extensions", target: "messages", kind: "project", note: "provider extensions are merged into the request." },
        ],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Provider semantic tests cover protocol-specific request plans and wire extensions." },
}
