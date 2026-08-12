import type { Agent, ModelRef } from "./agent.js"

/** spc_001 §4: the thin lowering IR between the public Agent contract and the Kernel.
 *  `tools`/`mcpServers`/`skills` all fold into `capabilities` — this layer does validation,
 *  normalization, and provider-extension pass-through only; no scheduling or governance logic. */
export interface AgentSpec {
  name: string
  instructions?: string
  model?: ModelRef
  capabilities: unknown[]
  providerOptions?: Record<string, unknown>
}

/** Pure: no branching on `providerOptions` contents, no vendor checks. Provider-specific handling
 *  belongs in the adapters that produce an `Agent`, never here. */
export function lowerAgent(agent: Agent): AgentSpec {
  const capabilities: unknown[] = [
    ...(agent.tools ?? []),
    ...(agent.mcpServers ?? []),
    ...(agent.skills ?? []),
  ]
  return {
    name: agent.name,
    instructions: agent.instructions,
    model: agent.model,
    capabilities,
    providerOptions: agent.providerOptions,
  }
}
