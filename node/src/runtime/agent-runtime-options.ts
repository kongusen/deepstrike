import type { AgentRunOptions } from "../agent-facade.js"
import type { GovernancePolicy } from "../governance.js"
import { createTextKnowledgeSource, type Knowledge } from "../knowledge/public.js"
import type { RuntimeOptions } from "./runner.js"
import { schemaInstruction } from "./output-schema.js"
import type { AgentDeclaration, AgentHostBindings } from "./agent-declaration.js"
import type { Skill } from "../skill.js"
import type { SubAgentRunContext } from "./sub-agent-orchestrator.js"
import type { SubAgentResult } from "../types/agent.js"

/** Resources already resolved by the facade; connection ownership stays with the Agent. */
export type AgentRuntimeResources = Pick<RuntimeOptions, "provider" | "executionPlane" | "sessionLog"> & {
  agentId: string
}

export interface AgentRuntimeOptionsRequest {
  declaration: AgentDeclaration
  bindings: AgentHostBindings
  options: AgentRunOptions
  resources: AgentRuntimeResources
}

/** The live public-to-host configuration adapter, shared by all facade execution entry points. */
export function buildAgentRuntimeOptions(
  request: AgentRuntimeOptionsRequest,
): RuntimeOptions {
  const { declaration, bindings, options, resources } = request
  const binding = bindings.runtimeBinding
  const governancePolicy = mergeGuardrailPolicies(binding?.runtimeOptions?.governancePolicy, declaration.guardrails)
  return {
    provider: resources.provider,
    ...((declaration.providerOptions || options.providerOptions) ? {
      extensions: { ...(declaration.providerOptions ?? {}), ...(options.providerOptions ?? {}) },
    } : {}),
    ...(declaration.capabilityFilter ? { capabilityFilter: declaration.capabilityFilter as RuntimeOptions["capabilityFilter"] } : {}),
    executionPlane: resources.executionPlane,
    // Declared/bound tools start visible; the kernel still applies the capability ceiling.
    baselineToolIds: resources.executionPlane.schemas().map(schema => schema.name),
    sessionLog: resources.sessionLog,
    maxTokens: declaration.maxTokens ?? 32_000,
    ...(declaration.instructions || declaration.outputSchema ? {
      systemPrompt: [
        declaration.instructions,
        declaration.outputSchema ? schemaInstruction(declaration.outputSchema) : undefined,
      ].filter((part): part is string => Boolean(part)).join("\n\n"),
    } : {}),
    ...(options.maxTurns !== undefined ? { maxTurns: options.maxTurns } : {}),
    ...(bindings.memoryStore ? { memoryStore: bindings.memoryStore } : {}),
    ...(bindings.memoryScope ? { memoryScope: bindings.memoryScope } : {}),
    ...(declaration.skills?.length ? { skillCatalog: declaration.skills as unknown as Skill[] } : {}),
    ...(!binding?.runtimeOptions?.knowledgeSource && declaration.knowledge?.some(item => item.source.kind === "text") ? {
      knowledgeSource: createTextKnowledgeSource((declaration.knowledge as Knowledge[])
        .filter((item): item is Knowledge & { source: { kind: "text"; content: string } } => item.source.kind === "text")
        .map(item => ({ id: item.id, name: item.name, content: item.source.content }))),
    } : {}),
    agentId: resources.agentId,
    ...(binding?.runtimeOptions ?? {}),
    // Host facilities may override catalogs, but must not discard the merged Agent guardrails.
    ...(governancePolicy ? { governancePolicy } : {}),
    ...(options.onPermissionRequest ? { onPermissionRequest: options.onPermissionRequest } : {}),
    ...(options.metadata ? { runMetadata: options.metadata } : {}),
    ...(binding?.resolveAgent ? {
      workflowAgentResolver: async (name: string, context: SubAgentRunContext): Promise<SubAgentResult | undefined> => {
        const target = await binding.resolveAgent?.(name)
        if (!target) return undefined
        const result = await target.run(context.spec.goal, {
          session: { id: context.spec.identity.sessionId },
          ...(context.abortSignal ? { signal: context.abortSignal } : {}),
        })
        return {
          agentId: context.spec.identity.agentId,
          result: {
            termination: result.status === "completed" ? "completed"
              : result.status === "cancelled" ? "user_abort" : "error",
            finalMessage: { role: "assistant", content: result.output, toolCalls: [] },
            turnsUsed: 0,
            totalTokensUsed: result.usage?.totalTokens ?? 0,
          },
        }
      },
    } : {}),
  }
}

function mergeGuardrailPolicies(
  base: GovernancePolicy | undefined,
  guardrails: AgentDeclaration["guardrails"] | undefined,
): GovernancePolicy | undefined {
  const policies = [base, ...(guardrails ?? []).map(guardrail => guardrail.policy)].filter(
    (policy): policy is GovernancePolicy => policy !== undefined,
  )
  if (!policies.length) return undefined
  return {
    ...(policies.some(policy => policy.defaultAction === "deny")
      ? { defaultAction: "deny" as const }
      : policies.some(policy => policy.defaultAction === "ask_user")
        ? { defaultAction: "ask_user" as const }
        : {}),
    rules: policies.flatMap(policy => policy.rules ?? []),
    vetoes: [...new Set(policies.flatMap(policy => policy.vetoes ?? []))],
    rateLimits: policies.flatMap(policy => policy.rateLimits ?? []),
    constraints: policies.flatMap(policy => policy.constraints ?? []),
    ...(policies.some(policy => policy.surfaceDeniedInSystem === false) ? { surfaceDeniedInSystem: false } : {}),
  }
}
