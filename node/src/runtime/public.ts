/** Runtime subpath: host execution primitives intentionally outside the root quick-start API. */
export { RuntimeRunner, collectText } from "./runner.js"
export type { RuntimeOptions } from "./runner.js"
export { runAgent, runFanout } from "./facade.js"
export type { RunAgentOptions, RunFanoutOptions } from "./facade.js"
export { InMemorySessionLog, FileSessionLog } from "./session-log.js"
export type { SessionLog, SessionEvent, SessionEventKind } from "./session-log.js"
export { projectAgentRun, projectAgentContext, projectAgentCapabilities, projectAgentGovernance, projectAgentDelegation } from "../agent-ir.js"
export type { AgentDescriptor } from "../agent-ir.js"
export {
  FileKernelJournal,
  InMemoryKernelJournal,
  JournalCasConflictError,
  JournalIntegrityError,
  JournalIoError,
} from "./kernel-journal.js"
export type {
  CheckpointCandidate,
  InstalledCheckpoint,
  JournalAppendReceipt,
  JournalEntry,
  JournalHead,
  JournalPruneReceipt,
  JournalRecordInput,
  KernelJournal,
} from "./kernel-journal.js"
export { diagnoseKernelJournal } from "./kernel-doctor.js"
export type { KernelJournalDiagnosis } from "./kernel-doctor.js"
export type { ContextPrepared } from "./context.js"
export { createContextPreparationAdapter, createNativeContextPreparationAdapter } from "./context.js"
export type { ContextPrepareJson, ContextVerifyJson, ContextProviderPreparationRequest } from "./context.js"
export { createEvolutionRuntimeAdapter, createNativeEvolutionRuntimeAdapter, EvolutionRuntime } from "./evolution.js"
export type { EvolutionStore } from "./evolution.js"
export { FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY, providerAttemptToRecord } from "./execution-evidence.js"
export type { InvocationOutcome, ModelInvocation, ProviderAttempt, ProviderAttemptRecord, ProviderAttemptStatus, UsageAccountingPolicy, ModelUsageSettlement } from "./execution-evidence.js"
