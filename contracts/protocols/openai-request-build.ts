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
      fields: {
        envelope: { source: ["input"], target: ["params"] },
        nested: [
          { source: "input.context", target: "params", kind: "project", note: "canonical messages and system text become Chat params." },
          { source: "input.tools", target: "params", kind: "project", note: "canonical tools become Chat tools." },
          { source: "input.extensions", target: "params", kind: "project", note: "dialect filters extensions into Chat params." },
          { source: "dialect", target: "params", kind: "state-effect", note: "dialect controls wire shape and cache behavior." },
        ],
        forbidden: [],
      },
    },
    {
      adapter: "providers/openai-responses-adapter:OpenAIResponsesAdapter.buildRequestAtBoundary",
      source: { type: "OpenAIResponsesRequestBuildRequest", layer: "host", authority: "host-runtime" },
      target: { type: "OpenAIResponsesRequestPlan", layer: "provider", authority: "provider" },
      fields: {
        envelope: { source: ["input", "state"], target: ["params"] },
        nested: [
          { source: "input.context", target: "params", kind: "project", note: "canonical messages become Responses input items." },
          { source: "input.tools", target: "params", kind: "project", note: "canonical tools and built-ins become Responses tools." },
          { source: "input.extensions", target: "params", kind: "project", note: "provider extensions become Responses params." },
          { source: "state", target: "params", kind: "state-effect", note: "previous response state becomes continuation params." },
        ],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "OpenAI request tests cover dialect preparation, continuation state, and native token-count plans.", testRefs: [{ path: "node/tests/openai-adapter.test.ts", selectors: ["OpenAIChatAdapter"] }, { path: "node/tests/openai-responses-adapter.test.ts", selectors: ["OpenAIResponsesAdapter"] }] },
}
