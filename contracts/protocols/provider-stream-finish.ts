/** Final provider stream response decoders used by the live streaming paths. */

import type { BoundaryProtocol } from "./types.js"

export const PROVIDER_STREAM_FINISH_PROTOCOL: BoundaryProtocol = {
  id: "provider-stream.finish",
  family: "host-provider",
  direction: "decode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "providers/anthropic-adapter:AnthropicMessagesAdapter.finishStreamAtBoundary",
      source: { type: "AnthropicStreamFinishRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/gemini-adapter:GeminiAdapter.finishStreamAtBoundary",
      source: { type: "GeminiStreamFinishRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["state", "final"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "state", target: "events", kind: "state-effect" }, { source: "final", target: "events", kind: "project" }], forbidden: [] },
    },
    {
      adapter: "providers/ollama-adapter:OllamaAdapter.finishStreamAtBoundary",
      source: { type: "OllamaStreamFinishRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["state", "final"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "state", target: "events", kind: "state-effect" }, { source: "final", target: "events", kind: "project" }], forbidden: [] },
    },
    {
      adapter: "providers/openai-chat:OpenAIChatAdapter.finishStreamAtBoundary",
      source: { type: "OpenAIChatStreamFinishRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
    {
      adapter: "providers/openai-responses-adapter:OpenAIResponsesAdapter.finishStreamAtBoundary",
      source: { type: "OpenAIResponsesStreamFinishRequest", layer: "provider", authority: "provider" },
      target: { type: "AdapterOutput", layer: "host", authority: "host-runtime" },
      fields: { envelope: { source: ["state"], target: ["events", "replay", "runStatePatch"] }, nested: [{ source: "state", target: "events", kind: "state-effect" }], forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Streaming tests cover terminal tool calls, usage, stop reasons, replay, and flush behavior.", testRefs: ["node/tests/tool-streaming.test.ts", "node/tests/provider-semantic-conformance.test.ts"] },
}
