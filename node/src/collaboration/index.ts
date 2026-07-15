// Contract types & builder
export type {
  AcceptanceCriterion,
  VerificationContract,
  ContractCheckResult,
} from "./contract.js"
export {
  ContractBuilder,
  formatContractForSystemPrompt,
  contractToCriteriaStrings,
} from "./contract.js"

// AgentPool
export { AgentPool } from "./pool.js"
export type { AgentRole, IsolatedVerifierContext } from "./pool.js"

export { CreatorVerifierBody, StructuredContractJudge } from "./harness.js"
export type { ContractOutcome } from "./harness.js"

// HandoffBus
export { HandoffBus } from "./handoff.js"
export type { HandoffArtifact, ContractOutcomeInput } from "./handoff.js"

// Collaboration modes
export { CreatorVerifierMode, OrchestrationMode } from "./modes/creator-verifier.js"
export type { CreatorVerifierMetrics } from "./modes/creator-verifier.js"
