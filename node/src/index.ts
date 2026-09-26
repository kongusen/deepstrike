// ╔══════════════════════════════════════════════════════════════════════════╗
// ║ @deepstrike/sdk — root surface (v0.2.74).                                      ║
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
  AgentRunOptions,
  AgentSession,
  DelegationRequest,
  DelegationResult,
  MemoryInput,
  RecallOptions,
  RunResult,
  SessionRef,
} from "./agent-facade.js"
export type { Agent } from "./agent-facade.js"
export type { AgentDeclaration } from "./agent-facade.js"

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
export type { GovernancePolicy, GovernanceConstraint } from "./governance.js"
export { governancePolicyPatch } from "./governance.js"
export type { LivePolicyPatch } from "./governance.js"
export type { SessionEvent, SessionEventKind } from "./session-events.js"
export { SESSION_EVENT_KINDS } from "./session-events.js"
export { InMemoryReactionCheckpointStore } from "./reactions.js"
export type {
  ReactionCheckpointClaim,
  ReactionCheckpointClaimResult,
  ReactionCheckpointReceipt,
  ReactionCheckpointStore,
  InMemoryReactionCheckpointStoreOptions,
} from "./reactions.js"

// ── Multi-agent primitive ───────────────────────────────────────────────────
// Parallel fan-out / sub-agent delegation. The full orchestration layer is in `@deepstrike/sdk/workflow`.
// ── Ecosystem Surface Contract (spc_001) ────────────────────────────────────
export type { ModelRef, ModelRequirement } from "./agent.js"
export type { Guardrail } from "./guardrail.js"
export type { MCPServer, McpTransport } from "./mcp-server.js"
export type { Knowledge, KnowledgeSourceRef } from "./knowledge/public.js"
export type { AgentRef, Handoff } from "./handoff-target.js"
export { createWorkflow } from "./workflow/definition.js"
export type { WorkflowDefinition, WorkflowStep, WorkflowResult } from "./workflow/definition.js"
export { evaluate } from "./evals/public.js"
export type { Dataset, DatasetCase, Evaluator, EvalResult, EvalRun, EvalTrace, ExecutionEvidence } from "./evals/public.js"

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
