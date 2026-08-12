import type { JsonSchema } from "./runtime/output-schema.js"

/** A control-transfer target, deliberately distinct from `HandoffArtifact` sprint evidence. */
export type AgentRef = string | { name: string }

export interface Handoff {
  agent: AgentRef
  description?: string
  inputSchema?: JsonSchema
  metadata?: Record<string, unknown>
}
