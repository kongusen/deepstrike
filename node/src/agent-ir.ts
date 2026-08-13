import { Agent, type AgentMemory, type AgentOptions, type ModelRef } from "./agent.js"
import type { Guardrail } from "./guardrail.js"
import type { Handoff } from "./handoff-target.js"
import type { Knowledge } from "./knowledge/public.js"
import type { MCPServer } from "./mcp-server.js"
import { WorkingMemory } from "./memory/public.js"
import type { JsonSchema } from "./runtime/output-schema.js"
import type { Skill } from "./skill.js"
import type { RegisteredTool } from "./tools/index.js"
import type { AgentCapabilityFilter } from "./types/agent.js"

export interface AgentToolDefinition {
  name: string
  description?: string
  parameters?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

/** A JSON-friendly Agent definition accepted by `normalizeAgent`.  It is deliberately declarative:
 * executable tools still enter the SDK through `AgentOptions.tools`. */
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

/** The host-facing lowering destinations. These are declarations, not grants: a runner still
 * attenuates them through its existing `AgentRunSpec.capabilityFilter` and mounted manifest. */
export interface AgentLoweringInputs {
  run: { name: string; model?: ModelRef }
  context: {
    description?: string
    instructions?: string
    outputSchema?: JsonSchema
    knowledge: Knowledge[]
  }
  capabilities: {
    tools: AgentToolIR[]
    mcpServers: MCPServer[]
    skills: Skill[]
    effective: AgentCapabilityIR[]
  }
  memory?: AgentMemoryIR
  delegation: { handoffs: Handoff[] }
  governance: { guardrails: Guardrail[] }
}

/** spc_015-09: the canonical, provider-neutral Agent IR between public SDK surfaces and host
 * runtime inputs. It contains neither an execution plan nor a Kernel wire DTO. */
export interface AgentSpec {
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
  /** Declared capabilities. This descriptive view grants nothing by itself. */
  capabilities: AgentCapabilityIR[]
  /** Host ceiling copied from the public Agent, when supplied. Empty axes remain non-narrowing. */
  capabilityFilter?: AgentCapabilityFilter
  /** The declarations that survive the supplied local ceiling. Host mounts may narrow further. */
  effectiveCapabilities: AgentCapabilityIR[]
  /** Namespace-isolated provider extensions. Unknown namespaces are preserved verbatim. */
  extensions: Record<string, unknown>
  inputs: AgentLoweringInputs
}

function clone<T>(value: T): T {
  if (value === undefined || value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone) as T
  const copy: Record<string, unknown> = {}
  for (const [key, nested] of Object.entries(value as Record<string, unknown>)) copy[key] = clone(nested)
  return copy as T
}

function toolDefinitionToRegisteredTool(tool: AgentToolDefinition): RegisteredTool {
  const parameters = clone(tool.parameters ?? { type: "object", properties: {} })
  if (parameters.type !== "object") {
    throw new Error(`tool "${tool.name}": parameters must be a JSON Schema with root type "object"`)
  }
  return {
    schema: {
      name: tool.name,
      description: tool.description ?? "",
      parameters: JSON.stringify(parameters),
    },
    providerOptions: clone(tool.providerOptions),
    async execute() {
      throw new Error(`tool "${tool.name}" has no execution binding — declarative Agent definitions only carry shape`)
    },
  }
}

function isRegisteredTool(tool: RegisteredTool | AgentToolDefinition): tool is RegisteredTool {
  return "schema" in tool && "execute" in tool
}

/** Normalizes native Agents and JSON-safe descriptor objects into the one public surface used by
 * lowering. It does not interpret provider namespaces or create executable capabilities. */
export function normalizeAgent(agent: Agent | AgentDefinition): Agent {
  if (agent instanceof Agent) return agent
  const tools = agent.tools?.map(tool => isRegisteredTool(tool) ? tool : toolDefinitionToRegisteredTool(tool))
  const { tools: _rawTools, ...options } = agent
  return new Agent({ ...options, ...(tools ? { tools } : {}) })
}

function lowerTool(tool: RegisteredTool): AgentToolIR {
  let parameters: unknown
  try {
    parameters = JSON.parse(tool.schema.parameters)
  } catch {
    throw new Error(`tool "${tool.schema.name}" has invalid JSON Schema parameters`)
  }
  if (!parameters || typeof parameters !== "object" || Array.isArray(parameters)) {
    throw new Error(`tool "${tool.schema.name}" parameters must decode to an object`)
  }
  return {
    name: tool.schema.name,
    description: tool.schema.description,
    parameters: clone(parameters as Record<string, unknown>),
    ...(tool.providerOptions ? { providerOptions: clone(tool.providerOptions) } : {}),
  }
}

function lowerMemory(memory: AgentMemory | undefined): AgentMemoryIR | undefined {
  if (!memory) return undefined
  if (memory instanceof WorkingMemory) return { kind: "working" }
  const reference = memory as { kind?: string; namespace?: string }
  return {
    kind: "durable",
    ...(reference.namespace ? { namespace: reference.namespace } : {}),
  }
}

function capabilityAllowed(capability: AgentCapabilityIR, filter: AgentCapabilityFilter | undefined): boolean {
  if (!filter) return true
  const kind = capability.kind === "mcp_server" ? "mcp_server" : capability.kind
  const kindAllowed = !filter.allowedKinds?.length || filter.allowedKinds.includes(kind)
  const idAllowed = !filter.allowedIds?.length || filter.allowedIds.includes(capability.id)
  return kindAllowed && idAllowed
}

/** Pure: no provider branching, no scheduling, authorization, persistence, or Kernel wire calls.
 * Providers consume only their own namespace from `extensions`; the host decides whether declared
 * capabilities survive its existing attenuation filter. */
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
