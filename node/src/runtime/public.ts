/** Runtime subpath: host execution primitives intentionally outside the root quick-start API. */
export { RuntimeRunner, collectText } from "./runner.js"
export { ContextManager, estimateContextTokens } from "./context-manager.js"
export type {
  ContextItem,
  ContextItemKind,
  ContextItemScope,
  ContextLedgerEvent,
  ContextLedgerEventKind,
  ContextManagerOptions,
  ContextSnapshot,
} from "./context-manager.js"
export type { RuntimeOptions } from "./runner.js"
export { runAgent, runFanout } from "./facade.js"
export type { RunAgentOptions, RunFanoutOptions } from "./facade.js"
export { InMemorySessionLog, FileSessionLog } from "./session-log.js"
export type { SessionLog, SessionEvent, SessionEventKind } from "./session-log.js"
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
export { FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY, providerAttemptToRecord } from "./execution-evidence.js"
export type { InvocationOutcome, ModelInvocation, ProviderAttempt, ProviderAttemptRecord, ProviderAttemptStatus, UsageAccountingPolicy, ModelUsageSettlement } from "./execution-evidence.js"
