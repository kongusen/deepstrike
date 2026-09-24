/** The live Agent facade-to-runtime configuration crossing. */

import type { BoundaryProtocol } from "./types.js"

export const AGENT_PUBLIC_TO_HOST_PROTOCOL: BoundaryProtocol = {
  id: "agent.public-to-host",
  family: "public-to-host",
  direction: "lower",
  fields: {
    drops: ["declaration", "bindings", "options", "resources"],
    derived: ["provider", "executionPlane", "sessionLog", "maxTokens", "systemPrompt", "agentId"],
    forbidden: [],
  },
  lazy: {
    lazySemantics: "none",
  },
  adapter: "runtime/agent-runtime-options:buildAgentRuntimeOptions",
  source: {
    type: "AgentRuntimeOptionsRequest",
    layer: "public",
    authority: "public-agent",
  },
  target: {
    type: "RuntimeOptions",
    layer: "host",
    authority: "host-runtime",
  },
  artifacts: {
    manifest: "contracts/manifests/agent-public-to-host.json",
  },
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Facade runtime-path tests cover provider, tool, governance, and binding behavior.", testRefs: [{ path: "node/tests/agent-runtime-path.test.ts", selectors: ["public Agent runtime path"] }, { path: "node/tests/agent-declaration.test.ts", selectors: ["snapshots declaration data before the caller mutates it"] }] },
}
