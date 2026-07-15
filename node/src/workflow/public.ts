// `@deepstrike/sdk/workflow` — multi-agent orchestration: the sub-agent host, reducers, spec builders,
// workflow node tools, agent/milestone types, and the collaboration (contract/handoff/mode) layer.
// The root package exports `runFanout`, `AgentPool`, `WorkflowSpec`/`WorkflowNodeSpec`; the advanced
// machinery lives here.
export { SubAgentOrchestrator, defaultSubAgentOrchestrator, spawnStandalone } from "../runtime/sub-agent-orchestrator.js"
export type { SubAgentRunContext } from "../runtime/sub-agent-orchestrator.js"
export { builtinReducers, resolveReducer } from "../runtime/reducers.js"
export type { Reducer, ReducerRegistry, ReducerInput } from "../runtime/reducers.js"
export { FileWorkflowStore } from "../runtime/workflow-store.js"

export {
  submitWorkflowNodesTool,
  startWorkflowTool,
  generateAndFilter,
  verifyRules,
  genEval,
  milestoneCheckPass,
  milestoneCheckFail,
} from "../types/agent.js"
export type {
  AgentCapabilityFilter,
  AgentIdentity,
  AgentIsolation,
  AgentRunSpec,
  AgentProcessChangedObservation,
  ContextInheritance,
  KernelAgentRole,
  LoopResult,
  MilestoneCheckResult,
  MilestoneContract,
  MilestonePhase,
  MilestonePolicy,
  SubAgentResult,
  TerminationReason,
  WorkflowSpawnInfo,
  WorkflowTaskSpec,
  WorkflowDependencyPolicy,
  WorkflowNodeStatus,
  WorkflowNodeOutcome,
  WorkflowOutcome,
} from "../types/agent.js"

// Collaboration layer (contracts, handoff, orchestration modes).
export type { AcceptanceCriterion, VerificationContract, ContractCheckResult } from "../collaboration/contract.js"
export { ContractBuilder, formatContractForSystemPrompt, contractToCriteriaStrings } from "../collaboration/contract.js"
export type { AgentRole, IsolatedVerifierContext, CoordinatorConfig } from "../collaboration/pool.js"
export type { ContractOutcome } from "../collaboration/harness.js"
export { HandoffBus } from "../collaboration/handoff.js"
export type { HandoffArtifact, ContractOutcomeInput } from "../collaboration/handoff.js"
export { CreatorVerifierMode, OrchestrationMode } from "../collaboration/modes/creator-verifier.js"
export type { CreatorVerifierMetrics } from "../collaboration/modes/creator-verifier.js"

// Skills loader + lower-level tool execution helpers.
export { scanSkillDir, readSkillFile } from "../skills/loader.js"
export type { SkillMetadata } from "../skills/loader.js"
export { executeTools, readFile, validateToolArguments } from "../tools/index.js"
export type { ToolExecContext } from "../tools/index.js"
