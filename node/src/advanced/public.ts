/**
 * Advanced escape hatch for runtime authors, diagnostics, and compatibility with the
 * implementation-oriented test harness. Ordinary applications should use the root Agent API.
 */
export * from "../index.js"
export { RuntimeRunner, collectText } from "../runtime/runner.js"
export type { RuntimeOptions } from "../runtime/runner.js"
export { runAgent, runFanout } from "../runtime/facade.js"
export type { RunAgentOptions, RunFanoutOptions } from "../runtime/facade.js"
export { LocalExecutionPlane } from "../runtime/execution-plane.js"
export type { ExecutionPlane, RunContext } from "../runtime/execution-plane.js"
export { InMemorySessionLog, FileSessionLog } from "../runtime/session-log.js"
export type { SessionLog, SessionEvent, SessionEventKind } from "../runtime/session-log.js"
export * from "../types/agent.js"
export * from "../runtime/run-group.js"
export * from "../runtime/event-stream.js"
export * from "../runtime/reliability.js"
export * from "../runtime/turn-policy.js"
export * from "../runtime/reactive-session.js"
export * from "../runtime/reaction-checkpoint.js"

export { projectAgentRun, projectAgentContext, projectAgentCapabilities, projectAgentGovernance, projectAgentDelegation } from "../agent-ir.js"
export type { AgentDescriptor } from "../agent-ir.js"
export {
  FileKernelJournal,
  InMemoryKernelJournal,
  JournalCasConflictError,
  JournalIntegrityError,
  JournalIoError,
  diagnoseKernelJournal,
} from "../runtime/public.js"
export type {
  CheckpointCandidate,
  InstalledCheckpoint,
  JournalAppendReceipt,
  JournalEntry,
  JournalHead,
  JournalPruneReceipt,
  JournalRecordInput,
  KernelJournal,
  KernelJournalDiagnosis,
  ContextPrepared,
  ContextPrepareJson,
  ContextVerifyJson,
  ContextProviderPreparationRequest,
  EvolutionRuntime,
  EvolutionStore,
  InvocationOutcome,
  ModelInvocation,
  ProviderAttempt,
  ProviderAttemptRecord,
  ProviderAttemptStatus,
  UsageAccountingPolicy,
  ModelUsageSettlement,
} from "../runtime/public.js"
