// `@deepstrike/sdk/providers` — backend provider factories, profiles, and provider-authoring types.
// The root package exports `createProvider` + the 3 base providers (Anthropic / OpenAI / OpenAIResponses);
// every other backend is a factory here. One function per backend (with a `protocol` option where a
// backend speaks both wires) replaces the old dual `<Backend>Provider`/`<Backend>AnthropicProvider` classes.
export { deepseek, kimi, qwen, glm, minimax, gemini, ollama } from "./factories.js"
export type { BackendProviderOptions } from "./factories.js"
// `OpenAIChatProvider` is the base OpenAI-compatible class advanced users compose/extend directly.
export { OpenAIChatProvider } from "./openai.js"
export { CircuitBreaker } from "./base.js"
export { OpenAIResponsesAdapter } from "./openai-responses.js"
export type {
  OpenAIResponsesRequestPlan,
  OpenAIResponsesRunState,
  OpenAIResponsesStreamState,
} from "./openai-responses.js"
export { OpenAIChatAdapter } from "./openai-chat.js"
export { GeminiAdapter } from "./gemini-adapter.js"
export { OllamaAdapter, OllamaNdjsonDecoder } from "./ollama-adapter.js"
export type {
  GeminiRequestPlan,
  GeminiStreamState,
} from "./gemini-adapter.js"
export { ProtocolResponseError } from "./protocol-adapter.js"
export type {
  AdapterDecodeInput,
  AdapterOutput,
  AdapterStreamInput,
  CanonicalStopReason,
  ProtocolAdapter,
} from "./protocol-adapter.js"
export { endpointProfiles } from "./endpoints.js"
export type { EndpointProfile, EndpointProfileId, EndpointProtocol, ProviderId } from "./endpoints.js"
export { modelRegistry, resolveEffectiveCapability } from "./model-registry.js"
export {
  ContentValidationError,
  ToolResultProjectionConflictError,
  normalizeCanonicalContext,
  normalizeCanonicalAdapterInput,
  normalizeToolResultPart,
  projectToolOutputToText,
  validateCanonicalAdapterInput,
} from "./content-normalization.js"
export type {
  CanonicalAdapterInput,
  CanonicalMessage,
  CanonicalMessageBlock,
  CanonicalRenderedContext,
  CanonicalToolResult,
} from "./content-normalization.js"
export type {
  CapabilityState,
  EffectiveCapability,
  EffectiveModelCapabilities,
  GenerationProtocol,
  ModelDescriptor,
  ModelKind,
  ModelRegistration,
  ResolvedProviderRuntime,
} from "./model-registry.js"
export type { ProviderRunState, ProviderToolSpec, ProviderReplay, RenderedContext, CacheBreakpointStrategy } from "../types.js"
