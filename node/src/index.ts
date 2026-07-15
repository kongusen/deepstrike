// ╔══════════════════════════════════════════════════════════════════════════╗
// ║ @deepstrike/sdk — root surface (v0.2.30).                                      ║
// ║                                                                            ║
// ║ This is the intent layer: run an agent, run a workflow, author a tool,     ║
// ║ pick a provider. Advanced machinery lives behind subpaths:                 ║
// ║   @deepstrike/sdk/providers  — backend provider classes + profiles         ║
// ║   @deepstrike/sdk/workflow   — orchestration, reducers, contracts, specs   ║
// ║   @deepstrike/sdk/planes     — worktree / sandbox / mcp / vpc planes        ║
// ║   @deepstrike/sdk/memory     — dream + working memory, knowledge sources    ║
// ║   @deepstrike/sdk/harness    — eval harnesses + judge                       ║
// ║   @deepstrike/sdk/os         — profiles, diagnostics, signals, replay tests ║
// ╚══════════════════════════════════════════════════════════════════════════╝

// ── Start here: the canonical entry points ─────────────────────────────────
export { runAgent, runFanout } from "./runtime/facade.js"
// ③ dynamic loop agents: self-pacing rounds over the kernel pacing trap.
export { runLoop, LoopDriver, foldLoopState } from "./runtime/loop-driver.js"
export type { LoopSpec, LoopOutcome } from "./runtime/loop-driver.js"
export type { RunAgentOptions, RunFanoutOptions } from "./runtime/facade.js"
export { RuntimeRunner, collectText } from "./runtime/runner.js"
export type { RuntimeOptions, KernelReliabilityOptions, OperationCancellationReason, PromptBudget, SchedulerPolicy } from "./runtime/runner.js"
export type { SignalPolicy } from "./runtime/os-profile.js"
export { readKernelDiagnostics, restoreKernelRuntime, snapshotKernelRuntime } from "./runtime/kernel-step.js"
export type { KernelDiagnostics, KernelSnapshot } from "./runtime/kernel-step.js"
export { rebuildKernelRuntime } from "./runtime/kernel-rebuild.js"
export type { KernelRebuildResult } from "./runtime/kernel-rebuild.js"
export {
  CONTEXT_POLICY_VERSION,
  DEFAULT_CONTEXT_POLICY_V1,
  PPM_SCALE,
  contextPolicyV1,
  normalizeContextPolicyV1,
  ratioToPpm,
} from "./runtime/context-policy.js"
export type {
  ContextPolicyOverridesV1,
  ContextPolicyV1,
  ContextPolicyWireV1,
  ContextPressureThresholdsV1,
} from "./runtime/context-policy.js"

// ── Execution plane + session log (the defaults) ────────────────────────────
export { LocalExecutionPlane } from "./runtime/execution-plane.js"
export type { ExecutionPlane, RunContext } from "./runtime/execution-plane.js"
export { InMemorySessionLog, FileSessionLog } from "./runtime/session-log.js"
export type { KernelTransactionEntry, SessionLog, SessionEvent } from "./runtime/session-log.js"
export {
  KERNEL_LOG_RECORD_VERSION,
  KernelLogConflictError,
  KernelLogIntegrityError,
  canonicalKernelJson,
  createKernelOperationGenesis,
  createKernelTransaction,
  kernelRecordDigest,
  verifyKernelOperationGenesis,
  verifyKernelTransaction,
  verifyKernelTransactionStream,
  verifyKernelTransactionSuccessor,
} from "./runtime/kernel-transaction-log.js"
export type {
  DurableAppendReceipt,
  KernelGenesisReceipt,
  KernelOperationCursor,
  KernelOperationGenesis,
  KernelOperationGenesisBody,
  KernelTransaction,
  KernelTransactionBody,
} from "./runtime/kernel-transaction-log.js"
export { InMemoryGroupBudgetStore, GroupBudgetScope } from "./runtime/run-group.js"
export type {
  RunGroup, GroupBudgetStore, GroupLedger, GroupCharge, GroupMember,
  GroupBudgetRequest, GroupBudgetReservation,
} from "./runtime/run-group.js"
export { InMemoryEventStream, isVisibleTo } from "./runtime/event-stream.js"
export type { EventStream, EventStreamOptions, BlackboardEvent, EventViewer } from "./runtime/event-stream.js"
export type { ObserverFailure, ObserverErrorHandler } from "./runtime/reliability.js"
export { ManagedTaskScope } from "./runtime/reliability.js"
export type { OperationContext, BackgroundTaskFailure, BackgroundTaskErrorHandler } from "./runtime/reliability.js"
export { reactByMention, directorDriven, roundRobin, firstNonEmpty, union } from "./runtime/turn-policy.js"
export type { TurnPolicy, PeerView } from "./runtime/turn-policy.js"
export { ReactiveSession, readRecentTool } from "./runtime/reactive-session.js"
export type { ReactiveSessionOptions, ReactivePeerSpec, EmitEvent, Reaction, ReactorTurn, ReactorContext } from "./runtime/reactive-session.js"
export { InMemoryReactionCheckpointStore, ReactionInProgressError } from "./runtime/reaction-checkpoint.js"
export type {
  ReactionCheckpointClaim,
  ReactionCheckpointClaimResult,
  ReactionCheckpointReceipt,
  ReactionCheckpointStore,
  ReactionRecord,
} from "./runtime/reaction-checkpoint.js"

// ── Tool authoring ──────────────────────────────────────────────────────────
export { tool, streamingTool } from "./tools/index.js"
export type { RegisteredTool, ToolExecContext } from "./tools/index.js"
export { safeTool, ok, fail, ToolError, formatToolError } from "./tools/errors.js"
export type { ToolEnvelope, ToolEnvelopeOk, ToolEnvelopeFail } from "./tools/errors.js"

// ── Providers (base classes + the universal factory) ────────────────────────
// Any backend — including a custom OpenAI-compatible endpoint — is reachable via `createProvider`.
// Backend-specific classes (DeepSeek/Kimi/Qwen/GLM/Gemini/Ollama/MiniMax) live in `@deepstrike/sdk/providers`.
export { AnthropicProvider } from "./providers/anthropic.js"
export type { AnthropicProviderConfig } from "./providers/anthropic.js"
export { OpenAIProvider } from "./providers/openai.js"
export type { OpenAIProviderOptions } from "./providers/openai.js"
export { OpenAIResponsesProvider } from "./providers/openai-responses.js"
export { createProvider } from "./providers/catalog.js"
export { UnsupportedModalityError } from "./providers/base.js"
export type { CreateProviderOptions, EndpointProfileId } from "./providers/catalog.js"

// ── Governance ──────────────────────────────────────────────────────────────
export { Governance } from "./governance.js"
export type { GovernanceVerdict, GovernancePolicy, GovernanceConstraint } from "./governance.js"

// ── Multi-agent primitive ───────────────────────────────────────────────────
// Parallel fan-out / sub-agent delegation. The full orchestration layer is in `@deepstrike/sdk/workflow`.
export { AgentPool } from "./collaboration/pool.js"

// ── Signals (the `RuntimeOptions.signalSource` surface) ─────────────────────
export type {
  RuntimeSignal,
  SignalClaim,
  SignalDeliveryReceipt,
  SignalSource,
} from "./signals/types.js"

// ── Core data types ─────────────────────────────────────────────────────────
export type {
  Message, ToolCall, ToolResult, ToolSchema,
  ContentPart, TextPart, ImagePart, AudioPart,
  StreamEvent, TextDelta, ThinkingDelta,
  ToolCallEvent, ToolChunk, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, ToolAuditFailedEvent, DoneEvent, ErrorEvent,
  PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions,
  LLMProvider, RetryConfig, TokenUsage,
} from "./types.js"
export type {
  WorkflowSpec,
  WorkflowNodeSpec,
  WorkflowDependencyPolicy,
  WorkflowNodeStatus,
  WorkflowNodeOutcome,
  WorkflowOutcome,
} from "./types/agent.js"
