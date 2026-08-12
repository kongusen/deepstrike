import {
  Agent,
  type AgentMemory,
  type AgentOptions,
  type AgentToolDefinition,
  type Guardrail,
  type Handoff,
  type Knowledge,
  type MCPServer,
  type MemoryReference,
  type ModelRef,
  type Skill,
} from "./agent.js"
import { WorkingMemory } from "./memory/index.js"
import type { AgentCapabilityFilter } from "./runtime/types/agent.js"
import type { RegisteredTool } from "./tools/index.js"

type JsonSchema = Record<string, unknown>

export interface AgentDefinition extends Omit<AgentOptions, "tools"> {
  tools?: Array<RegisteredTool | AgentToolDefinition>
}

export interface AgentToolIR {
  name: string
  description: string
  parameters: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

export interface AgentMemoryIR {
  kind: "durable" | "working"
  namespace?: string
}

export type AgentCapabilityIR =
  | { kind: "tool"; id: string; description: string }
  | { kind: "mcp_server"; id: string; description: string }
  | { kind: "skill"; id: string; description: string }

export interface AgentLoweringInputs {
  run: { name: string; model?: ModelRef }
  context: { description?: string; instructions?: string; outputSchema?: JsonSchema; knowledge: Knowledge[] }
  capabilities: { tools: AgentToolIR[]; mcpServers: MCPServer[]; skills: Skill[]; effective: AgentCapabilityIR[] }
  memory?: AgentMemoryIR
  delegation: { handoffs: Handoff[] }
  governance: { guardrails: Guardrail[] }
}

/** Provider-neutral host descriptor. This is neither an execution plan nor a Kernel wire DTO. */
export interface AgentSpec {
  version: 1
  name: string
  description?: string
  instructions?: string
  model?: ModelRef
  tools: AgentToolIR[]
  outputSchema?: JsonSchema
  mcpServers?: MCPServer[]
  skills?: Skill[]
  memory?: AgentMemoryIR
  knowledge?: Knowledge[]
  handoffs?: Handoff[]
  guardrails?: Guardrail[]
  metadata?: Record<string, unknown>
  capabilities: AgentCapabilityIR[]
  capabilityFilter?: AgentCapabilityFilter
  effectiveCapabilities: AgentCapabilityIR[]
  extensions: Record<string, unknown>
  /** @deprecated Read `extensions`; retained as an additive compatibility alias. */
  providerOptions?: Record<string, unknown>
  inputs: AgentLoweringInputs
}

function clone<T>(value: T): T {
  if (value === undefined || value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone) as T
  const copy: Record<string, unknown> = {}
  for (const [key, nested] of Object.entries(value as Record<string, unknown>)) copy[key] = clone(nested)
  return copy as T
}

function isRegisteredTool(tool: RegisteredTool | AgentToolDefinition): tool is RegisteredTool {
  return "schema" in tool && "execute" in tool
}

/** Native Agents need no normalization; JSON-safe declarations remain schema-only capabilities. */
export function normalizeAgent(agent: Agent | AgentDefinition): Agent {
  if (agent instanceof Agent) return agent
  return new Agent({ ...agent, ...(agent.tools ? { tools: clone(agent.tools) } : {}) })
}

function lowerTool(tool: RegisteredTool | AgentToolDefinition): AgentToolIR {
  if (!isRegisteredTool(tool)) {
    const parameters = clone(tool.parameters ?? { type: "object", properties: {} })
    if (parameters.type !== "object") {
      throw new Error(`tool "${tool.name}": parameters must be a JSON Schema with root type "object"`)
    }
    return {
      name: tool.name,
      description: tool.description ?? "",
      parameters,
      ...(tool.providerOptions ? { providerOptions: clone(tool.providerOptions) } : {}),
    }
  }
  let parameters: unknown
  try {
    parameters = JSON.parse(tool.schema.parameters)
  } catch {
    throw new Error(`tool "${tool.schema.name}" has invalid JSON Schema parameters`)
  }
  if (!parameters || typeof parameters !== "object" || Array.isArray(parameters) || (parameters as Record<string, unknown>).type !== "object") {
    throw new Error(`tool "${tool.schema.name}" parameters must decode to an object`)
  }
  return { name: tool.schema.name, description: tool.schema.description, parameters: clone(parameters as Record<string, unknown>) }
}

function lowerMemory(memory: AgentMemory | undefined): AgentMemoryIR | undefined {
  if (!memory) return undefined
  if (memory instanceof WorkingMemory) return { kind: "working" }
  const reference = memory as MemoryReference
  return { kind: "durable", ...(reference.namespace ? { namespace: reference.namespace } : {}) }
}

function capabilityAllowed(capability: AgentCapabilityIR, filter: AgentCapabilityFilter | undefined): boolean {
  if (!filter) return true
  const kindAllowed = !filter.allowedKinds?.length || filter.allowedKinds.includes(capability.kind)
  const idAllowed = !filter.allowedIds?.length || filter.allowedIds.includes(capability.id)
  return kindAllowed && idAllowed
}

/** Pure lowering only: providers, Kernel scheduling, persistence, and grants remain outside it. */
export function lowerAgent(agent: Agent): AgentSpec {
  const tools = (agent.tools ?? []).map(lowerTool)
  const mcpServers = clone(agent.mcpServers ?? [])
  const skills = clone(agent.skills ?? [])
  const knowledge = clone(agent.knowledge ?? [])
  const handoffs = clone(agent.handoffs ?? [])
  const guardrails = clone(agent.guardrails ?? [])
  const memory = lowerMemory(agent.memory)
  const extensions = clone(agent.providerOptions ?? {})
  const capabilities: AgentCapabilityIR[] = [
    ...tools.map(tool => ({ kind: "tool" as const, id: tool.name, description: tool.description })),
    ...mcpServers.map(server => ({
      kind: "mcp_server" as const,
      id: server.name ?? server.transport.kind,
      description: server.name ?? `${server.transport.kind} MCP server`,
    })),
    ...skills.map(skill => ({ kind: "skill" as const, id: skill.name, description: skill.description ?? "" })),
  ]
  const capabilityFilter = agent.capabilityFilter ? clone(agent.capabilityFilter) : undefined
  const effectiveCapabilities = capabilities.filter(capability => capabilityAllowed(capability, capabilityFilter))
  return {
    version: 1,
    name: agent.name,
    ...(agent.description ? { description: agent.description } : {}),
    ...(agent.instructions ? { instructions: agent.instructions } : {}),
    ...(agent.model ? { model: clone(agent.model) } : {}),
    tools,
    ...(agent.outputSchema ? { outputSchema: clone(agent.outputSchema) } : {}),
    ...(mcpServers.length ? { mcpServers } : {}),
    ...(skills.length ? { skills } : {}),
    ...(memory ? { memory } : {}),
    ...(knowledge.length ? { knowledge } : {}),
    ...(handoffs.length ? { handoffs } : {}),
    ...(guardrails.length ? { guardrails } : {}),
    ...(agent.metadata ? { metadata: clone(agent.metadata) } : {}),
    capabilities,
    ...(capabilityFilter ? { capabilityFilter } : {}),
    effectiveCapabilities,
    extensions,
    providerOptions: clone(extensions),
    inputs: {
      run: { name: agent.name, ...(agent.model ? { model: clone(agent.model) } : {}) },
      context: {
        ...(agent.description ? { description: agent.description } : {}),
        ...(agent.instructions ? { instructions: agent.instructions } : {}),
        ...(agent.outputSchema ? { outputSchema: clone(agent.outputSchema) } : {}),
        knowledge,
      },
      capabilities: { tools, mcpServers, skills, effective: effectiveCapabilities },
      ...(memory ? { memory } : {}),
      delegation: { handoffs },
      governance: { guardrails },
    },
  }
}
