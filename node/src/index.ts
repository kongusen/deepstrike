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
export type { RuntimeOptions } from "./runtime/runner.js"

// ── Execution plane + session log (the defaults) ────────────────────────────
export { LocalExecutionPlane } from "./runtime/execution-plane.js"
export type { ExecutionPlane, RunContext } from "./runtime/execution-plane.js"
export { InMemorySessionLog, FileSessionLog } from "./runtime/session-log.js"
export type { SessionLog, SessionEvent } from "./runtime/session-log.js"
export { InMemoryGroupBudgetStore, SessionLogGroupBudgetStore } from "./runtime/run-group.js"
export type { RunGroup, GroupBudgetStore, GroupLedger, GroupCharge, GroupMember } from "./runtime/run-group.js"
export { InMemoryEventStream, isVisibleTo } from "./runtime/event-stream.js"
export type { EventStream, BlackboardEvent, EventViewer } from "./runtime/event-stream.js"
export { reactByMention, directorDriven, roundRobin, firstNonEmpty, union } from "./runtime/turn-policy.js"
export type { TurnPolicy, PeerView } from "./runtime/turn-policy.js"
export { ReactiveSession, readRecentTool } from "./runtime/reactive-session.js"
export type { ReactiveSessionOptions, ReactivePeerSpec, EmitEvent, Reaction, ReactorTurn, ReactorContext } from "./runtime/reactive-session.js"

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
export type { CreateProviderOptions, EndpointProfileId } from "./providers/catalog.js"

// ── Governance ──────────────────────────────────────────────────────────────
export { Governance } from "./governance.js"
export type { GovernanceVerdict, GovernancePolicy, GovernanceConstraint } from "./governance.js"

// ── Multi-agent primitive ───────────────────────────────────────────────────
// Parallel fan-out / sub-agent delegation. The full orchestration layer is in `@deepstrike/sdk/workflow`.
export { AgentPool } from "./collaboration/pool.js"

// ── Signals (the `RuntimeOptions.signalSource` surface) ─────────────────────
export type { RuntimeSignal, SignalSource } from "./signals/types.js"

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
} from "./types/agent.js"
