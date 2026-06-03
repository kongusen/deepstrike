// ── Runtime (Layer 1.5) ────────────────────────────────────────────────────
export { RuntimeRunner, collectText } from "./runtime/runner.js"
export type { RuntimeOptions, SchedulerBudget } from "./runtime/runner.js"
export type { MemoryWriteRateLimit, ResourceQuota } from "./kernel.js"
export { KernelPrimitivesDashboard } from "./runtime/kernel-primitives-dashboard.js"
export { FilteredExecutionPlane } from "./runtime/filtered-plane.js"
export { SubAgentOrchestrator, defaultSubAgentOrchestrator, spawnStandalone } from "./runtime/sub-agent-orchestrator.js"
export type { SubAgentRunContext } from "./runtime/sub-agent-orchestrator.js"
export { LocalExecutionPlane } from "./runtime/execution-plane.js"
export type { ExecutionPlane, RunContext } from "./runtime/execution-plane.js"
export { InMemorySessionLog, FileSessionLog } from "./runtime/session-log.js"
export type { SessionLog, SessionEvent } from "./runtime/session-log.js"
export {
  DEFAULT_NATIVE_ATTENTION_POLICY,
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
  assertNativeProfile,
  osProfile,
} from "./runtime/os-profile.js"
export type { NativeOsProfile, OsProfileId } from "./runtime/os-profile.js"
export {
  rebuildOsSnapshotFromSessionEvents,
  sessionLogHasRequiredCategories,
} from "./runtime/os-snapshot.js"
export type { OsSnapshot } from "./runtime/os-snapshot.js"
export { categoryForKind, kernelObservationToSessionEvent } from "./runtime/kernel-event-log.js"
export type { KernelEventCategory } from "./runtime/kernel-event-log.js"
export { NullArchiveStore, FileArchiveStore } from "./runtime/archive.js"
export type { ArchiveStore } from "./runtime/archive.js"
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
  MemoryWriteRequest, MemoryQuery, MemoryRetrieval, MemoryMetadata, MemoryKind,
} from "./memory/protocols.js"

// ── Knowledge & Signals ────────────────────────────────────────────────────
export type { KnowledgeSource } from "./knowledge/source.js"
export { ScheduledPrompt } from "./signals/scheduled.js"
export { SignalGateway } from "./signals/gateway.js"
export type { RuntimeSignal, SignalSource } from "./signals/types.js"

// ── Safety & Governance ────────────────────────────────────────────────────
export { PermissionManager, PermissionMode } from "./safety/permissions.js"
export type { PermissionDecision, Permission } from "./safety/permissions.js"
export { Governance, governancePolicyToKernelEvent } from "./governance.js"
export type { GovernanceVerdict, GovernancePolicy, GovernanceConstraint } from "./governance.js"

// ── Harness ────────────────────────────────────────────────────────────────
export { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "./harness/harness.js"
export type { HarnessRequest, HarnessOutcome, HarnessLoopOptions, QualityGate } from "./harness/harness.js"

// ── Types ──────────────────────────────────────────────────────────────────
export type {
  Message, ToolCall, ToolResult, ToolSchema,
  ContentPart, TextPart, ImagePart, AudioPart,
  StreamEvent, TextDelta, ThinkingDelta,
  ToolCallEvent, ToolChunk, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  LLMProvider, RetryConfig, TokenUsage, ProviderToolSpec, ProviderRunState, ProviderReplay,
  RenderedContext,
} from "./types.js"
export type {
  AgentCapabilityFilter,
  AgentIdentity,
  AgentIsolation,
  AgentRunSpec,
  AgentProcessChangedObservation,
  ContextInheritance,
  KernelAgentRole,
  LoopResult,
  MilestoneCheckResult,
  MilestoneContract,
  MilestonePhase,
  MilestonePolicy,
  SubAgentResult,
  TerminationReason,
} from "./types/agent.js"
export {
  agentIdentitySub,
  agentRunSpecToKernel,
  milestoneCheckFail,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  subAgentResultToKernel,
} from "./types/agent.js"

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
export type { AgentRole, IsolatedVerifierContext, CoordinatorConfig } from "./collaboration/pool.js"
export { KERNEL_ROLE_MAP } from "./collaboration/pool.js"
export { ContractDrivenHarness } from "./collaboration/harness.js"
export type { ContractOutcome, ContractHarnessOptions, Violation } from "./collaboration/harness.js"
export { HandoffBus } from "./collaboration/handoff.js"
export type { HandoffArtifact, ContractOutcomeInput } from "./collaboration/handoff.js"
export { CreatorVerifierMode, OrchestrationMode } from "./collaboration/modes/creator-verifier.js"
export type { CreatorVerifierMetrics } from "./collaboration/modes/creator-verifier.js"
