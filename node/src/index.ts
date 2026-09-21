// ╔══════════════════════════════════════════════════════════════════════════╗
// ║ @deepstrike/sdk — root surface (v0.2.72).                                      ║
// ║                                                                            ║
// ║ This is the intent layer: run an agent, run a workflow, author a tool,     ║
// ║ pick a provider. Advanced machinery lives behind subpaths:                 ║
// ║   @deepstrike/sdk/providers  — backend provider classes + profiles         ║
// ║   @deepstrike/sdk/workflow   — orchestration, reducers, contracts, specs   ║
// ║   @deepstrike/sdk/planes     — worktree / sandbox / mcp / vpc planes        ║
// ║   @deepstrike/sdk/memory     — durable + working memory, knowledge sources  ║
// ║   @deepstrike/sdk/harness    — eval harnesses + judge                       ║
// ║   @deepstrike/sdk/os         — profiles, diagnostics, signals, replay tests ║
// ╚══════════════════════════════════════════════════════════════════════════╝

// ── Start here: the canonical entry points ─────────────────────────────────
// ③ dynamic loop agents: self-pacing rounds over the kernel pacing trap.
export { createAgent } from "./agent-facade.js"
export type {
  AgentDefinition,
  AgentRunOptions,
  AgentSession,
  AgentRuntime,
  DelegationRequest,
  DelegationResult,
  MemoryInput,
  RecallOptions,
  RunResult,
  SessionRef,
} from "./agent-facade.js"
export { collectText } from "./runtime/runner.js"
// Self-Harness H1 instruction/nudge surfaces named on `RuntimeOptions`; the full manifest API lives
// on the `@deepstrike/sdk/harness` subpath.
export type { InstructionProfile, NudgeRule, NudgeTrigger } from "./harness/public.js"
export type { SignalPolicy } from "./runtime/os-profile.js"
export {
  DEFAULT_CONTEXT_POLICY,
  PPM_SCALE,
  contextPolicy,
  normalizeContextPolicy,
  ratioToPpm,
} from "./runtime/context-policy.js"
export type {
  ContextPolicyOverrides,
  ContextPolicy,
  ContextPolicyWire,
  ContextPressureThresholds,
} from "./runtime/context-policy.js"

// ── Execution plane + session log (the defaults) ────────────────────────────
export type { SessionEvent, SessionEventKind } from "./runtime/session-log.js"
// Registered session-event vocabulary (F9/S3; manifest-pinned by sdk-conformance, P7-S4)
export { SESSION_EVENT_KINDS } from "./runtime/session-log.js"
// ── content-parts-v1 registered encoding (F14/B5; byte-pinned by sdk-conformance) ──
export {
  CANONICAL_CONTENT_PARTS_PREFIX,
  encodeCanonicalContentParts,
  decodeCanonicalContentParts,
} from "./runtime/kernel-step.js"
// ── Durable transaction capability (Canonical Kernel ABI §9.1) ──────────────
export { InMemoryGroupBudgetStore, GroupBudgetScope } from "./runtime/run-group.js"
export type {
  RunGroup, GroupBudgetStore, GroupLedger, GroupCharge, GroupMember,
  GroupBudgetRequest, GroupBudgetReservation,
} from "./runtime/run-group.js"
export { InMemoryEventStream, isVisibleTo } from "./runtime/event-stream.js"
export type { EventStream, EventStreamOptions, BlackboardEvent, EventViewer } from "./runtime/event-stream.js"
export type { ObserverFailure, ObserverErrorHandler } from "./runtime/reliability.js"
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
export { createProvider, createProviderAsync, resolveProviderRuntime, resolveProviderRuntimeAsync } from "./providers/catalog.js"
export { UnsupportedModalityError } from "./providers/base.js"
export type { CreateProviderOptions, EndpointProfileId } from "./providers/catalog.js"
export {
  VERIFIABLE_REPORT_SCHEMA,
  VERIFIABLE_FORK_SCHEMA,
  assertVerifiableReportSchema,
  createVerifiableRuntimeAdapter,
  createNativeVerifiableRuntimeAdapter,
  VerifiableOperation,
} from "./runtime/verifiable-report.js"
export type {
  VerifiableCommand,
  CheckVerdict,
  VerifiableForkManifest,
  ForkPlan,
  VerifiableEvidence,
  VerifyOptions,
  ReplayOptions,
  VerifiableRuntimeAdapter,
  VerifiableReport,
  VerifiableOperationJson,
} from "./runtime/verifiable-report.js"
export type { GovernancePolicy, GovernanceConstraint } from "./governance.js"

// ── Multi-agent primitive ───────────────────────────────────────────────────
// Parallel fan-out / sub-agent delegation. The full orchestration layer is in `@deepstrike/sdk/workflow`.
// ── Ecosystem Surface Contract (spc_001) ────────────────────────────────────
export { Agent } from "./agent.js"
export type { AgentOptions, AgentMemory, MemoryReference, ModelRef, ModelRequirement } from "./agent.js"
export { lowerAgent, normalizeAgent } from "./agent-ir.js"
export type { AgentCapabilityIR, AgentLoweringInputs, AgentMemoryIR, AgentSpec, AgentToolDefinition, AgentToolIR } from "./agent-ir.js"
export type { Guardrail } from "./guardrail.js"
export type { MCPServer, McpTransport } from "./mcp-server.js"
export type { Knowledge, KnowledgeSourceRef } from "./knowledge/public.js"
export { createTextKnowledgeSource } from "./knowledge/public.js"
export type { TextKnowledgeDocument } from "./knowledge/public.js"
export { agentRefName } from "./handoff-target.js"
export type { AgentRef, Handoff } from "./handoff-target.js"
export type { Session } from "./session.js"
export { createWorkflow } from "./workflow/definition.js"
export type { WorkflowDefinition, WorkflowStep, WorkflowResult } from "./workflow/definition.js"
export { evaluate } from "./evals/public.js"
export type { Dataset, DatasetCase, Evaluator, EvalResult, EvalRun, EvalTrace } from "./evals/public.js"

// ── Signals (the `RuntimeOptions.signalSource` surface) ─────────────────────
export type {
  RuntimeSignal,
  SignalClaim,
  SignalDeliveryReceipt,
  SignalSource,
} from "./signals/types.js"

// ── Core data types ─────────────────────────────────────────────────────────
export type {
  ModelMessage, RuntimeMessage, StoredMessage, WireMessage, ToolCall, ToolExecutionResult, ToolSchema,
  ContentPart, TextPart, ImagePart, AudioPart,
  MediaSource, ContentBlockText, ContentBlockImage, ContentBlockAudio,
  ContentBlockVideo, ContentBlockFile,
  StreamEvent, TextDelta, ThinkingDelta,
  ToolCallEvent, ToolChunk, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, ToolAuditFailedEvent, DoneEvent, ErrorEvent,
  PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions,
  LLMProvider, RetryConfig, TokenUsage,
  ProviderWireEvidence, ProviderTransportTelemetry,
} from "./types.js"
export {
  DurableContentError,
  decodeDurableContent,
  decodeDurableToolResult,
  encodeDurableContent,
  encodeDurableToolResult,
  toolOutputBlocksToDurable,
  durableBlocksToToolOutput,
} from "./runtime/durable-content.js"
export type { DurableContent, DurableContentBlock, DurableSource, DurableToolResult } from "./runtime/durable-content.js"
export type {
  WorkflowSpec,
  WorkflowNodeSpec,
  SchedulingFactors,
  WorkflowDependencyPolicy,
  WorkflowNodeStatus,
  WorkflowNodeOutcome,
  WorkflowOutcome,
} from "./types/agent.js"
