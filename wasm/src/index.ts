// @deepstrike/wasm — semantic public root. Runtime machinery lives in ./advanced.
export { Agent, createAgent } from "./agent.js"
export type {
  AgentDefinition, AgentOptions, AgentRunResult, PortableRunResult, PortableSession, AgentToolDefinition, AgentMemory, AgentRef,
  Guardrail, Handoff, Knowledge, KnowledgeSourceRef, MCPServer, McpTransport, MemoryReference,
  ModelRef, ModelRequirement, RuntimeBinding, Skill,
} from "./agent.js"
export { AnthropicProvider } from "./providers/anthropic.js"
export { OpenAIProvider, qwen, deepseek, minimax, kimi } from "./providers/openai.js"
export type { OpenAIProviderOptions, BackendProviderOptions } from "./providers/openai.js"
export { ProviderError, classifyProviderError } from "./providers/provider-error.js"
export type { ProviderErrorKind, ProviderErrorOptions } from "./providers/provider-error.js"
export { tool, executeTools } from "./tools/index.js"
export type { RegisteredTool, ToolExecContext } from "./tools/index.js"
export { safeTool, ok, fail, ToolError, formatToolError } from "./tools/errors.js"
export type { ToolEnvelope, ToolEnvelopeOk, ToolEnvelopeFail } from "./tools/errors.js"
export { WorkingMemory } from "./memory/index.js"
export { DurableMemory } from "./memory/durable.js"
export { InMemoryMemoryStore } from "./memory/in-memory-store.js"
export type { InMemoryMemoryStoreOptions } from "./memory/in-memory-store.js"
export type { MemoryStore, Memory, MemorySearchOptions, SessionStore, SessionData, SessionMessage, MemoryRecord, MemoryRecall, MemoryRecallLifecycle, MemoryQuery, MemoryScope, MemoryProvenance, MemoryKind, MemoryAuthor, MemoryTrustLevel } from "./memory/index.js"
export type { KnowledgeSource } from "./knowledge/index.js"
export type { GovernancePolicy, GovernanceConstraint } from "./governance.js"
export type {
  ModelMessage, ToolCall, ToolExecutionResult, ToolSchema, RenderedContext, ProviderRunState,
  StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, ToolResultEvent, ToolAuditFailedEvent,
  DoneEvent, ErrorEvent, PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions, LLMProvider,
  CacheBreakpointStrategy, ProviderWireEvidence, ProviderTransportTelemetry,
} from "./types.js"
export { contentDispositionFor, requireContentDisposition } from "./providers/content-policy.js"
export type { ContentDisposition, ContentPlacement, InputModality } from "./providers/content-policy.js"
