import type { AgentDefinition, AgentRunOptions } from "../agent-facade.js"
import type { GovernancePolicy } from "../governance.js"
import { createTextKnowledgeSource, type Knowledge } from "../knowledge/public.js"
import type { RuntimeOptions } from "./runner.js"
import { schemaInstruction } from "./output-schema.js"

/** Resources already resolved by the facade; connection ownership stays with the Agent. */
export type AgentRuntimeResources = Pick<RuntimeOptions, "provider" | "executionPlane" | "sessionLog"> & {
  agentId: string
}

/** The live public-to-host configuration adapter, shared by all facade execution entry points. */
export function buildAgentRuntimeOptions(
  definition: Readonly<AgentDefinition>,
  options: AgentRunOptions,
  resources: AgentRuntimeResources,
): RuntimeOptions {
  const binding = definition.runtimeBinding
  return {
    provider: resources.provider,
    ...(mergeGuardrailPolicies(binding?.runtimeOptions?.governancePolicy, definition.guardrails)
      ? { governancePolicy: mergeGuardrailPolicies(binding?.runtimeOptions?.governancePolicy, definition.guardrails) }
      : {}),
    ...(definition.capabilityFilter ? { capabilityFilter: definition.capabilityFilter } : {}),
    executionPlane: resources.executionPlane,
    // Declared/bound tools start visible; the kernel still applies the capability ceiling.
    baselineToolIds: resources.executionPlane.schemas().map(schema => schema.name),
    sessionLog: resources.sessionLog,
    maxTokens: definition.maxTokens ?? 32_000,
    ...(definition.instructions || definition.outputSchema ? {
      systemPrompt: [
        definition.instructions,
        definition.outputSchema ? schemaInstruction(definition.outputSchema) : undefined,
      ].filter((part): part is string => Boolean(part)).join("\n\n"),
    } : {}),
    ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
    ...(definition.memoryStore ? { memoryStore: definition.memoryStore } : {}),
    ...(definition.memoryScope ? { memoryScope: definition.memoryScope } : {}),
    ...(definition.skills?.length ? { skillCatalog: definition.skills } : {}),
    ...(!binding?.runtimeOptions?.knowledgeSource && definition.knowledge?.some(item => item.source.kind === "text") ? {
      knowledgeSource: createTextKnowledgeSource(definition.knowledge
        .filter((item): item is Knowledge & { source: { kind: "text"; content: string } } => item.source.kind === "text")
        .map(item => ({ id: item.id, name: item.name, content: item.source.content }))),
    } : {}),
    agentId: resources.agentId,
    ...(binding?.runtimeOptions ?? {}),
    ...(options.onPermissionRequest ? { onPermissionRequest: options.onPermissionRequest } : {}),
  }
}

function mergeGuardrailPolicies(
  base: GovernancePolicy | undefined,
  guardrails: AgentDefinition["guardrails"] | undefined,
): GovernancePolicy | undefined {
  const policies = [base, ...(guardrails ?? []).map(guardrail => guardrail.policy)].filter(
    (policy): policy is GovernancePolicy => policy !== undefined,
  )
  if (!policies.length) return undefined
  return {
    ...(policies.some(policy => policy.defaultAction === "deny") ? { defaultAction: "deny" as const } : {}),
    rules: policies.flatMap(policy => policy.rules ?? []),
    vetoes: [...new Set(policies.flatMap(policy => policy.vetoes ?? []))],
    rateLimits: policies.flatMap(policy => policy.rateLimits ?? []),
    constraints: policies.flatMap(policy => policy.constraints ?? []),
    ...(policies.some(policy => policy.surfaceDeniedInSystem === false) ? { surfaceDeniedInSystem: false } : {}),
  }
}
