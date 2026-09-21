import type { Agent } from "../agent.js"
import { agentRefName } from "../handoff-target.js"
import type { AgentRef } from "../handoff-target.js"
import type { WorkflowDependencyPolicy, WorkflowOutcome, WorkflowSpec } from "../types/agent.js"

export interface WorkflowStep {
  agent: Agent | AgentRef
  input: string
  dependsOn?: string[]
  dependencyPolicy?: WorkflowDependencyPolicy
  metadata?: Record<string, unknown>
}

export interface WorkflowDefinition {
  name?: string
  steps: Record<string, WorkflowStep>
  metadata?: Record<string, unknown>
}

export function lowerWorkflowDefinition(definition: WorkflowDefinition): WorkflowSpec {
  const entries = Object.entries(definition.steps)
  const indexById = new Map(entries.map(([nodeId], index) => [nodeId, index]))
  return {
    nodes: entries.map(([nodeId, step]) => ({
      ...(() => {
        const dependsOn = step.dependsOn?.map(dependencyId => {
          const index = indexById.get(dependencyId)
          if (index === undefined) throw new Error(`workflow step "${nodeId}" depends on unknown step "${dependencyId}"`)
          return index
        })
        return dependsOn ? { dependsOn } : {}
      })(),
      nodeId,
      task: { goal: step.input },
      role: "implement",
      isolation: "read_only",
      contextInheritance: "system_only",
      dependencyPolicy: step.dependencyPolicy,
      agent: agentRefName(step.agent),
    })),
  }
}

export type WorkflowResult = WorkflowOutcome

export function createWorkflow(definition: WorkflowDefinition): WorkflowDefinition {
  return Object.freeze({ ...definition, steps: Object.freeze({ ...definition.steps }) })
}
