/** Stateful provider stream chunk decoders used by the live streaming paths. */

import type { BoundaryProtocol } from "./types.js"

export const PROVIDER_STREAM_DECODE_PROTOCOL: BoundaryProtocol = {
  id: "provider-stream.decode",
  family: "host-provider",
  direction: "decode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "providers/anthropic-adapter:AnthropicMessagesAdapter.pushStreamChunkAtBoundary",
      source: { type: "AnthropicStreamChunkRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["chunk", "state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "chunk", target: "events", kind: "project" }, { source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/gemini-adapter:GeminiAdapter.pushStreamChunkAtBoundary",
      source: { type: "GeminiStreamChunkRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["chunk", "state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "chunk", target: "events", kind: "project" }, { source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/ollama-adapter:OllamaAdapter.pushStreamChunkAtBoundary",
      source: { type: "OllamaStreamChunkRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["chunk", "state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "chunk", target: "events", kind: "project" }, { source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/openai-chat:OpenAIChatAdapter.pushStreamChunkAtBoundary",
      source: { type: "OpenAIChatStreamChunkRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["chunk", "state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "chunk", target: "events", kind: "project" }, { source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/openai-responses-adapter:OpenAIResponsesAdapter.pushStreamChunkAtBoundary",
      source: { type: "OpenAIResponsesStreamChunkRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["chunk", "state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "chunk", target: "events", kind: "project" }, { source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Streaming tests cover emitted deltas, tool calls, usage, and provider state updates.", testRefs: [{ path: "node/tests/tool-streaming.test.ts", selectors: ["streaming tools"] }, { path: "node/tests/provider-semantic-conformance.test.ts", selectors: ["SPC-028-11/13 route identity changes with execution target"] }] },
}
