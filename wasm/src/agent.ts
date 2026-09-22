import type { AgentCapabilityFilter } from "./runtime/types/agent.js"
import type { Memory, WorkingMemory } from "./memory/index.js"
import type { RegisteredTool } from "./tools/index.js"
import type { LLMProvider, StreamEvent } from "./types.js"
import { RuntimeRunner, type RuntimeOptions } from "./runtime/runner.js"
import { LocalExecutionPlane } from "./runtime/execution-plane.js"
import { InMemorySessionLog } from "./runtime/session-log.js"

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

export interface AgentDefinition {
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

export interface RuntimeBinding {
  provider?: LLMProvider
  providerFor?: (model: string) => LLMProvider | undefined
  runtimeOptions?: Partial<RuntimeOptions>
}

export type AgentOptions = AgentDefinition

export interface AgentRunResult {
  output: string
  sessionId: string
  status: "completed" | "partial" | "failed" | "cancelled"
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
  readonly definition: Readonly<AgentDefinition>
  private readonly binding?: RuntimeBinding

  constructor(options: AgentOptions, binding?: RuntimeBinding) {
    if ("runtimeBinding" in options) throw new Error("pass runtime binding as the second Agent argument")
    const definition = options
    this.definition = Object.freeze({ ...definition })
    this.binding = binding
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

  async run(goal: string, options: { sessionId?: string; maxTurns?: number } = {}): Promise<AgentRunResult> {
    const binding = this.binding
    const provider = binding?.provider ?? (typeof this.model === "string" ? binding?.providerFor?.(this.model) : undefined)
    if (!provider) throw new Error(`agent "${this.name}" has no runtime provider binding`)
    const runtime = new RuntimeRunner({
      provider,
      executionPlane: new LocalExecutionPlane(),
      sessionLog: new InMemorySessionLog(),
      maxTokens: 32_000,
      agentId: this.name,
      ...(this.instructions ? { systemPrompt: this.instructions } : {}),
      ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
      ...(binding?.runtimeOptions ?? {}),
    } as RuntimeOptions)
    const sessionId = options.sessionId ?? `session-${crypto.randomUUID()}`
    const events: StreamEvent[] = []
    for await (const event of runtime.run({ sessionId, goal })) events.push(event)
    const output = events.filter(event => event.type === "text_delta").map(event => ("delta" in event ? String(event.delta) : "")).join("")
    const failed = events.some(event => event.type === "error")
    return { output, sessionId, status: failed ? "failed" : "completed" }
  }

  stream(goal: string, options: { sessionId?: string } = {}): AsyncIterable<StreamEvent> {
    const binding = this.binding
    const provider = binding?.provider ?? (typeof this.model === "string" ? binding?.providerFor?.(this.model) : undefined)
    if (!provider) throw new Error(`agent "${this.name}" has no runtime provider binding`)
    const runtime = new RuntimeRunner({
      provider,
      executionPlane: new LocalExecutionPlane(),
      sessionLog: new InMemorySessionLog(),
      maxTokens: 32_000,
      agentId: this.name,
      ...(this.instructions ? { systemPrompt: this.instructions } : {}),
      ...(binding?.runtimeOptions ?? {}),
    } as RuntimeOptions)
    return runtime.run({ sessionId: options.sessionId ?? `session-${crypto.randomUUID()}`, goal })
  }
}

export function createAgent(options: AgentOptions, binding?: RuntimeBinding): Agent {
  return new Agent(options, binding)
}
