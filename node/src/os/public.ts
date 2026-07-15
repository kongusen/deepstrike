// `@deepstrike/sdk/os` — Agent-OS diagnostics, profiles, signal/permission machinery, replay-testing,
// and the scheduler/quota/policy types referenced by advanced `RuntimeOptions` fields.
export {
  DEFAULT_NATIVE_SIGNAL_POLICY,
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
  assertNativeProfile,
  osProfile,
} from "../runtime/os-profile.js"
export type { NativeOsProfile, OsProfileId, SignalPolicy } from "../runtime/os-profile.js"
export { rebuildOsSnapshotFromSessionEvents } from "../runtime/os-snapshot.js"
export type { OsSnapshot } from "../runtime/os-snapshot.js"
export type { KernelEventCategory } from "../runtime/kernel-event-log.js"
export { KernelPrimitivesDashboard } from "../runtime/kernel-primitives-dashboard.js"
export type { MemoryPolicy, MemoryWriteRateLimit, ResourceQuota } from "../kernel.js"
export type { PromptBudget, SchedulerPolicy } from "../runtime/runner.js"

// Signals + SDK-side permissions.
export { ScheduledPrompt } from "../signals/scheduled.js"
export { SignalGateway } from "../signals/gateway.js"
export { PermissionManager, PermissionMode } from "../safety/permissions.js"
export type { PermissionDecision, Permission } from "../safety/permissions.js"

// Replay-based testing utilities.
export { ReplayProvider } from "../runtime/replay-provider.js"
export type { ReplayProviderOpts } from "../runtime/replay-provider.js"
export { extractRecordedMessages } from "../runtime/replay-fixture.js"
export { ProviderReplayValidationError, DEGRADED_REASONING_PLACEHOLDER } from "../providers/replay-validator.js"
export {
  assessProviderReplayability,
  peekProviderReplay,
  seedProviderReplayFromEvents,
  isReplayCompatibleWithProvider,
} from "../runtime/provider-replay.js"
export type { ReplayabilityAssessment } from "../types.js"
