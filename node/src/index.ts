// ── Runtime (Layer 1.5) ────────────────────────────────────────────────────
export { RuntimeRunner, collectText } from "./runtime/runner.js"
export type { RuntimeOptions } from "./runtime/runner.js"
export { LocalExecutionPlane } from "./runtime/execution-plane.js"
export type { ExecutionPlane, RunContext } from "./runtime/execution-plane.js"
export { InMemorySessionLog, FileSessionLog } from "./runtime/session-log.js"
export type { SessionLog, SessionEvent } from "./runtime/session-log.js"
export { EnvCredentialVault, InMemoryCredentialVault, ChainedCredentialVault } from "./runtime/credential-vault.js"
export type { CredentialVault } from "./runtime/credential-vault.js"
export { ProcessSandboxPlane } from "./runtime/process-sandbox-plane.js"
export type { SandboxOptions } from "./runtime/process-sandbox-plane.js"
export { McpProxyPlane } from "./runtime/mcp-proxy-plane.js"
export type { McpServerConfig } from "./runtime/mcp-proxy-plane.js"
export { RemoteVpcPlane } from "./runtime/remote-vpc-plane.js"
export type { RemoteVpcOptions } from "./runtime/remote-vpc-plane.js"

// ── Providers ─────────────────────────────────────────────────────────────
export { AnthropicProvider } from "./providers/anthropic.js"
export { OpenAIChatProvider, OpenAIProvider } from "./providers/openai.js"
export { DeepSeekProvider } from "./providers/deepseek.js"
export { KimiProvider } from "./providers/kimi.js"
export { QwenProvider } from "./providers/qwen.js"
export { GeminiProvider } from "./providers/gemini.js"
export { MiniMaxProvider } from "./providers/minimax.js"
export { OllamaProvider } from "./providers/ollama.js"
export { CircuitBreaker, normalizeToolCall } from "./providers/base.js"
export { OpenAIChatAdapter } from "./providers/openai-chat.js"
export { OpenAIResponsesAdapter, OpenAIResponsesProvider } from "./providers/openai-responses.js"
export type { OpenAIResponsesRunState } from "./providers/openai-responses.js"
export { endpointProfiles, modelProfiles, getModelProfile } from "./providers/profiles.js"
export type { ModelProfileId, ProviderId } from "./providers/profiles.js"
export { createProvider } from "./providers/catalog.js"
export type { CreateProviderOptions, EndpointProfileId } from "./providers/catalog.js"

// ── Tools & Skills ─────────────────────────────────────────────────────────
export { tool, streamingTool, executeTools, readFile, validateToolArguments } from "./tools/index.js"
export type { RegisteredTool } from "./tools/index.js"
export { scanSkillDir, readSkillFile } from "./skills/loader.js"
export type { SkillMetadata } from "./skills/loader.js"

// ── Memory ─────────────────────────────────────────────────────────────────
export { WorkingMemory } from "./memory/working.js"
export type {
  DreamStore, DreamResult, SessionData, SessionMessage, MemoryEntry, CurationResult, CurationStats,
} from "./memory/protocols.js"

// ── Knowledge & Signals ────────────────────────────────────────────────────
export type { KnowledgeSource } from "./knowledge/source.js"
export { ScheduledPrompt } from "./signals/scheduled.js"
export { SignalGateway } from "./signals/gateway.js"
export type { RuntimeSignal, SignalSource } from "./signals/types.js"

// ── Safety & Governance ────────────────────────────────────────────────────
export { PermissionManager, PermissionMode } from "./safety/permissions.js"
export type { PermissionDecision, Permission } from "./safety/permissions.js"
export { Governance } from "./governance.js"
export type { GovernanceVerdict } from "./governance.js"

// ── Harness ────────────────────────────────────────────────────────────────
export { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "./harness/harness.js"
export type { HarnessRequest, HarnessOutcome, HarnessLoopOptions, QualityGate } from "./harness/harness.js"

// ── Types ──────────────────────────────────────────────────────────────────
export type {
  Message, ToolCall, ToolResult, ToolSchema,
  ContentPart, TextPart, ImagePart, AudioPart,
  StreamEvent, TextDelta, ThinkingDelta,
  ToolCallEvent, ToolChunk, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent,
  LLMProvider, RetryConfig, TokenUsage, ProviderToolSpec, ProviderRunState, ProviderReplay,
  RenderedContext,
} from "./types.js"

// ── Collaboration layer (Layer 2 + Layer 3) ────────────────────────────────
export type {
  AcceptanceCriterion,
  VerificationContract,
  ContractCheckResult,
} from "./collaboration/contract.js"
export {
  ContractBuilder,
  formatContractForSystemPrompt,
  contractToCriteriaStrings,
} from "./collaboration/contract.js"
export { AgentPool } from "./collaboration/pool.js"
export type { AgentRole, IsolatedVerifierContext } from "./collaboration/pool.js"
export { ContractDrivenHarness } from "./collaboration/harness.js"
export type { ContractOutcome, ContractHarnessOptions, Violation } from "./collaboration/harness.js"
export { HandoffBus } from "./collaboration/handoff.js"
export type { HandoffArtifact, ContractOutcomeInput } from "./collaboration/handoff.js"
export { CreatorVerifierMode, OrchestrationMode } from "./collaboration/modes/creator-verifier.js"
export type { CreatorVerifierMetrics } from "./collaboration/modes/creator-verifier.js"
