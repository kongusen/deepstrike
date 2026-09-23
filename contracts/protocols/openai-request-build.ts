/** Stateful OpenAI request builders exposed as one typed boundary input. */

import type { BoundaryProtocol } from "./types.js"

export const OPENAI_REQUEST_BUILD_PROTOCOL: BoundaryProtocol = {
  id: "openai-request.build",
  family: "host-provider",
  direction: "encode",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "providers/openai-chat:OpenAIChatAdapter.buildRequestAtBoundary",
      source: { type: "OpenAIChatRequestBuildRequest", layer: "host", authority: "host-runtime" },
      target: { type: "OpenAIChatRequestPlan", layer: "provider", authority: "provider" },
      fields: { envelope: { source: ["input"], target: ["params"] }, forbidden: [] },
    },
    {
      adapter: "providers/openai-responses-adapter:OpenAIResponsesAdapter.buildRequestAtBoundary",
      source: { type: "OpenAIResponsesRequestBuildRequest", layer: "host", authority: "host-runtime" },
      target: { type: "OpenAIResponsesRequestPlan", layer: "provider", authority: "provider" },
      fields: { envelope: { source: ["input", "state"], target: ["params"] }, forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "OpenAI request tests cover dialect preparation, continuation state, and native token-count plans." },
}
