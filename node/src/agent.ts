import type { RegisteredTool } from "./tools/index.js"
import type { WorkingMemory } from "./memory/public.js"
import type { MCPServer } from "./mcp-server.js"
import type { JsonSchema } from "./runtime/output-schema.js"
import type { Guardrail } from "./guardrail.js"

/** spc_001 §2.1: dual-mode model reference — either an explicit vendor model name, or a
 *  capability-based requirement the Host routes to a concrete model. Routing logic for the
 *  `ModelRequirement` branch is Provider Adapter work, out of scope here (spc_001-06). */
export type ModelRef = string | ModelRequirement

export interface ModelRequirement {
  capability?: { reasoning?: boolean; vision?: boolean; toolUse?: boolean }
  contextWindow?: number
  latencyClass?: string
  costClass?: string
}

export interface AgentOptions {
  name: string
  description?: string
  instructions?: string
  model?: ModelRef
  tools?: RegisteredTool[]
  mcpServers?: MCPServer[]
  skills?: unknown[] // placeholder, see spc_001-04
  memory?: WorkingMemory
  knowledge?: unknown[] // placeholder
  handoffs?: unknown[] // placeholder
  providerOptions?: Record<string, unknown>
  outputSchema?: JsonSchema
  metadata?: Record<string, unknown>
  guardrails?: Guardrail[]
}

/** spc_001 §2.1: public Agent contract — a thin field-storage wrapper today, with lowering to the
 *  Kernel's `AgentRunSpec`/Canonical Agent IR added incrementally by later cards (spc_001-03+). */
export class Agent {
  readonly name: string
  readonly description?: string
  readonly instructions?: string
  readonly model?: ModelRef
  readonly tools?: RegisteredTool[]
  readonly mcpServers?: MCPServer[]
  readonly skills?: unknown[]
  readonly memory?: WorkingMemory
  readonly knowledge?: unknown[]
  readonly handoffs?: unknown[]
  readonly providerOptions?: Record<string, unknown>
  readonly outputSchema?: JsonSchema
  readonly metadata?: Record<string, unknown>
  readonly guardrails?: Guardrail[]

  constructor(options: AgentOptions) {
    this.name = options.name
    this.description = options.description
    this.instructions = options.instructions
    this.model = options.model
    this.tools = options.tools
    this.mcpServers = options.mcpServers
    this.skills = options.skills
    this.memory = options.memory
    this.knowledge = options.knowledge
    this.handoffs = options.handoffs
    this.providerOptions = options.providerOptions
    this.outputSchema = options.outputSchema
    this.metadata = options.metadata
    this.guardrails = options.guardrails
  }
}
