import type { AgentCapabilityFilter } from "./runtime/types/agent.js"
import type { Memory, WorkingMemory } from "./memory/index.js"
import type { RegisteredTool } from "./tools/index.js"

type JsonSchema = Record<string, unknown>

export interface MemoryReference {
  kind?: "durable"
  namespace?: string
}

export type AgentMemory = Memory | WorkingMemory | MemoryReference
export type ModelRef = string | ModelRequirement

export interface ModelRequirement {
  capability?: { reasoning?: boolean; vision?: boolean; toolUse?: boolean }
  contextWindow?: number
  latencyClass?: string
  costClass?: string
}

export interface AgentToolDefinition {
  name: string
  description?: string
  parameters?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export type McpTransport =
  | { kind: "stdio"; command: string; args?: string[] }
  | { kind: "http"; url: string }
  | { kind: "sse"; url: string }
  | { kind: "custom"; [key: string]: unknown }

export interface MCPServer {
  name?: string
  transport: McpTransport
  tools?: string[]
  resources?: boolean
  prompts?: boolean
  auth?: Record<string, unknown>
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export interface Skill {
  name: string
  description?: string
  instructions?: string
  resources?: unknown[]
  scripts?: unknown[]
  tools?: unknown[]
  mcpServers?: unknown[]
  knowledge?: unknown[]
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export type KnowledgeSourceRef =
  | { kind: "file"; path: string }
  | { kind: "directory"; path: string }
  | { kind: "text"; content: string }
  | { kind: "url"; url: string }
  | { kind: "vector"; retriever: unknown }
  | { kind: "custom"; [key: string]: unknown }

export interface Knowledge {
  id?: string
  name?: string
  source: KnowledgeSourceRef
  description?: string
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export type AgentRef = string | { name: string }

export interface Handoff {
  agent: AgentRef
  description?: string
  inputSchema?: JsonSchema
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export interface Guardrail {
  name: string
  description?: string
  metadata?: Record<string, unknown>
}

export interface AgentOptions {
  name: string
  description?: string
  instructions?: string
  model?: ModelRef
  capabilityFilter?: AgentCapabilityFilter
  tools?: Array<RegisteredTool | AgentToolDefinition>
  mcpServers?: MCPServer[]
  skills?: Skill[]
  memory?: AgentMemory
  knowledge?: Knowledge[]
  handoffs?: Handoff[]
  providerOptions?: Record<string, unknown>
  outputSchema?: JsonSchema
  metadata?: Record<string, unknown>
  guardrails?: Guardrail[]
}

export class Agent {
  readonly name: string
  readonly description?: string
  readonly instructions?: string
  readonly model?: ModelRef
  readonly capabilityFilter?: AgentCapabilityFilter
  readonly tools?: Array<RegisteredTool | AgentToolDefinition>
  readonly mcpServers?: MCPServer[]
  readonly skills?: Skill[]
  readonly memory?: AgentMemory
  readonly knowledge?: Knowledge[]
  readonly handoffs?: Handoff[]
  readonly providerOptions?: Record<string, unknown>
  readonly outputSchema?: JsonSchema
  readonly metadata?: Record<string, unknown>
  readonly guardrails?: Guardrail[]

  constructor(options: AgentOptions) {
    this.name = options.name
    this.description = options.description
    this.instructions = options.instructions
    this.model = options.model
    this.capabilityFilter = options.capabilityFilter
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
