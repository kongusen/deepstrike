import { WorkingMemory } from "../memory/working.js"
import type { AgentDefinition, RuntimeBinding } from "../agent-facade.js"
import type { RegisteredTool } from "../tools/index.js"
import type { AgentMemory } from "../agent.js"
import type { MemoryScope, MemoryStore } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"

type DeepReadonly<T> = T extends (...args: never[]) => unknown ? never
  : T extends readonly (infer Item)[] ? readonly DeepReadonly<Item>[]
  : T extends object ? { readonly [Key in keyof T]: DeepReadonly<T[Key]> }
  : T

type DeclarationData = Omit<AgentDefinition, "runtimeBinding" | "memoryStore" | "memoryScope" | "tools" | "memory" | "knowledge"> & {
  name: string
  tools?: Pick<RegisteredTool, "schema" | "providerOptions">[]
  memory?: { kind: "working"; binding?: "runtime" } | { kind: "durable"; namespace?: string; binding?: "runtime" }
  knowledge?: Array<Omit<NonNullable<AgentDefinition["knowledge"]>[number], "source"> & {
    source: Exclude<NonNullable<AgentDefinition["knowledge"]>[number]["source"], { kind: "vector" }> | { kind: "vector" }
  }>
}

/** Immutable, JSON-serializable configuration captured when createAgent is called. */
export type AgentDeclaration = DeepReadonly<DeclarationData>

/** Executable and host-owned objects are held apart from the public declaration. */
export interface AgentHostBindings {
  runtimeBinding?: RuntimeBinding
  memoryStore?: MemoryStore
  memoryScope?: MemoryScope
  memory?: AgentMemory
  tools: RegisteredTool[]
  vectorRetrievers: Map<number, KnowledgeSource>
}

function copyData<T>(value: T, path = "declaration", ancestors = new WeakSet<object>()): T {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value
  if (typeof value === "number" && Number.isFinite(value)) return value
  if (typeof value !== "object") throw new TypeError(`${path} must contain JSON data`)
  if (ancestors.has(value)) throw new TypeError(`${path} contains a cycle`)
  const prototype = Object.getPrototypeOf(value)
  if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null) {
    throw new TypeError(`${path} must contain plain JSON objects`)
  }
  ancestors.add(value)
  try {
    if (Array.isArray(value)) {
      return value.map((item, index) => copyData(item, `${path}[${index}]`, ancestors)) as T
    }
    const result: Record<string, unknown> = {}
    for (const [key, item] of Object.entries(value)) {
      if (item !== undefined) result[key] = copyData(item, `${path}.${key}`, ancestors)
    }
    return result as T
  } finally {
    ancestors.delete(value)
  }
}

function freezeData<T>(value: T): T {
  if (value && typeof value === "object") {
    for (const item of Object.values(value)) freezeData(item)
    Object.freeze(value)
  }
  return value
}

function captureBinding(binding: RuntimeBinding | undefined): RuntimeBinding | undefined {
  if (!binding) return undefined
  const options = binding.runtimeOptions
  return {
    ...binding,
    ...(options ? { runtimeOptions: {
      ...options,
      ...(options.memoryPolicy ? { memoryPolicy: copyData(options.memoryPolicy) } : {}),
      ...(options.governancePolicy ? { governancePolicy: copyData(options.governancePolicy) } : {}),
      ...(options.signalPolicy ? { signalPolicy: copyData(options.signalPolicy) } : {}),
      ...(options.resourceQuota ? { resourceQuota: copyData(options.resourceQuota) } : {}),
      ...(options.initialMemory ? { initialMemory: copyData(options.initialMemory) } : {}),
      ...(options.skillCatalog ? { skillCatalog: copyData(options.skillCatalog) } : {}),
    } } : {}),
  }
}

export function captureAgentDeclaration(input: AgentDefinition): { declaration: AgentDeclaration; bindings: AgentHostBindings } {
  if (Boolean(input.memoryStore) !== Boolean(input.memoryScope)) {
    throw new TypeError("agent memory requires memoryStore and memoryScope to be bound together")
  }
  const vectorRetrievers = new Map<number, KnowledgeSource>()
  const knowledge = input.knowledge?.map((item, index) => {
    if (item.source.kind !== "vector") return item
    vectorRetrievers.set(index, item.source.retriever)
    return { ...item, source: { kind: "vector" as const } }
  })
  const memory = input.memory
    ? input.memory instanceof WorkingMemory ? { kind: "working" as const, ...(input.memoryStore ? { binding: "runtime" as const } : {}) }
      : "search" in input.memory ? { kind: "durable" as const, ...("namespace" in input.memory ? { namespace: input.memory.namespace } : {}), ...(input.memoryStore ? { binding: "runtime" as const } : {}) }
        : { kind: "durable" as const, ...(input.memory.namespace ? { namespace: input.memory.namespace } : {}), ...(input.memoryStore ? { binding: "runtime" as const } : {}) }
    : input.memoryStore && input.memoryScope
      ? { kind: "durable" as const, namespace: input.memoryScope.namespace, binding: "runtime" as const }
      : undefined
  const raw: DeclarationData = {
    name: input.name ?? "agent",
    description: input.description,
    instructions: input.instructions,
    model: input.model,
    capabilityFilter: input.capabilityFilter,
    tools: input.tools?.map(({ schema, providerOptions }) => ({ schema, providerOptions })),
    mcpServers: input.mcpServers,
    skills: input.skills,
    memory,
    knowledge,
    handoffs: input.handoffs,
    providerOptions: input.providerOptions,
    outputSchema: input.outputSchema,
    metadata: input.metadata,
    guardrails: input.guardrails,
    maxTokens: input.maxTokens,
  }
  const declaration = freezeData(copyData(raw)) as AgentDeclaration
  const bindings: AgentHostBindings = {
    runtimeBinding: captureBinding(input.runtimeBinding),
    memoryStore: input.memoryStore,
    memoryScope: input.memoryScope ? copyData(input.memoryScope) : undefined,
    memory: input.memory,
    tools: (input.tools ?? []).map(tool => ({
      schema: copyData(tool.schema),
      ...(tool.providerOptions ? { providerOptions: copyData(tool.providerOptions) } : {}),
      execute: tool.execute.bind(tool),
    })),
    vectorRetrievers,
  }
  return { declaration, bindings }
}
