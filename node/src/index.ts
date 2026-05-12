export { Agent } from "./agent.js"
export type { AgentOptions } from "./agent.js"
export { AnthropicProvider } from "./providers/anthropic.js"
export { OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider, KimiProvider } from "./providers/openai.js"
export { OllamaProvider } from "./providers/ollama.js"
export { CircuitBreaker, normalizeToolCall } from "./providers/base.js"
export { tool, executeTools, readFile } from "./tools/index.js"
export type { RegisteredTool } from "./tools/index.js"
export { scanSkillDir, readSkillFile } from "./skills/loader.js"
export type { SkillMetadata } from "./skills/loader.js"
export { WorkingMemory } from "./memory/working.js"
export type {
  DreamStore, DreamResult, SessionData, SessionMessage, MemoryEntry, CurationResult, CurationStats,
} from "./memory/protocols.js"
export type { KnowledgeSource } from "./knowledge/source.js"
export { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "./harness/harness.js"
export type { HarnessRequest, HarnessOutcome, HarnessLoopOptions, QualityGate } from "./harness/harness.js"
export { ScheduledPrompt } from "./signals/scheduled.js"
export { SignalGateway } from "./signals/gateway.js"
export type { RuntimeSignal, SignalSource } from "./signals/types.js"
export { PermissionManager, PermissionMode } from "./safety/permissions.js"
export type { PermissionDecision, Permission } from "./safety/permissions.js"
// Kernel Governance — full pipeline (Permission → Veto → RateLimit → Constraint → Audit)
// @deepstrike/core is a CJS native addon; static ESM named re-export doesn't work,
// so we load it via createRequire and re-export with proper types preserved.
import { createRequire } from "module"
const _cjsRequire = createRequire(import.meta.url)
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const Governance: typeof import("@deepstrike/core").Governance = _cjsRequire("@deepstrike/core").Governance
export type { GovernanceVerdict } from "@deepstrike/core"
export type {
  Message, ToolCall, ToolResult, ToolSchema,
  ContentPart, TextPart, ImagePart, AudioPart,
  StreamEvent, TextDelta, ThinkingDelta,
  ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent,
  LLMProvider, RetryConfig, TokenUsage, ProviderToolSpec,
} from "./types.js"
