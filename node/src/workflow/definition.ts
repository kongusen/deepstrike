import type { Agent } from "../agent.js"
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
  return {
    nodes: Object.entries(definition.steps).map(([nodeId, step]) => ({
      nodeId,
      task: { goal: step.input },
      role: "execute",
      isolation: "read_only",
      contextInheritance: "system_only",
      dependsOn: step.dependsOn,
      dependencyPolicy: step.dependencyPolicy,
      agent: typeof step.agent === "string" ? step.agent : step.agent.name,
    } as never)),
  }
}

export type WorkflowResult = WorkflowOutcome

export function createWorkflow(definition: WorkflowDefinition): WorkflowDefinition {
  return Object.freeze({ ...definition, steps: Object.freeze({ ...definition.steps }) })
}
