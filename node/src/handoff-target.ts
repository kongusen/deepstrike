import type { JsonSchema } from "./runtime/output-schema.js"

/** A control-transfer target, deliberately distinct from `HandoffArtifact` sprint evidence. */
export type AgentRef = string | { name: string }

/** Canonical lowering primitive shared by handoff authorization and workflow nodes. */
export function agentRefName(ref: AgentRef): string {
  const name = typeof ref === "string" ? ref : ref.name
  if (!name) throw new Error("agent reference requires a non-empty name")
  return name
}

export interface Handoff {
  agent: AgentRef
  description?: string
  inputSchema?: JsonSchema
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}
