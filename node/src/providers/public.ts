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
export { openAIChatDialects } from "./openai-chat-dialects.js"
export type { OpenAIChatDialectId, OpenAIChatWireDialect } from "./openai-chat-dialects.js"
export { GeminiAdapter } from "./gemini-adapter.js"
export { OllamaAdapter, OllamaNdjsonDecoder } from "./ollama-adapter.js"
export type {
  GeminiRequestPlan,
  GeminiStreamState,
} from "./gemini-adapter.js"
export { ProtocolResponseError } from "./protocol-adapter.js"
export {
  ProviderError,
  classifyProviderError,
} from "./provider-error.js"
export type { ProviderErrorKind, ProviderErrorOptions } from "./provider-error.js"
export type {
  AdapterDecodeInput,
  AdapterOutput,
  AdapterStreamInput,
  CanonicalStopReason,
  ProtocolAdapter,
} from "./protocol-adapter.js"
export { endpointProfiles } from "./endpoints.js"
export type { EndpointProfile, EndpointProfileId, EndpointProtocol, ProviderId } from "./endpoints.js"
export { ContentPolicyError, contentDispositionFor } from "./content-policy.js"
export type { ContentDisposition, ContentPlacement } from "./content-policy.js"
export { CapabilityRouter } from "./capability-router.js"
export type { CapabilityRequirement, CapabilityRouteResult } from "./capability-router.js"
export { createProviderRequestPlan, measurementForPlan, normalizeProviderUsage, priceProviderUsage, recordPromptMeasurement } from "./request-plan.js"
export type { CostObservation, NormalizedProviderUsage, PricingSnapshot, ProviderRequestEndpoint, ProviderRequestPlan, RecordedPromptMeasurement } from "./request-plan.js"
export { CredentialResolutionError, redactCredential, resolveCredential } from "./credentials.js"
export type { CredentialOptions, CredentialRequest, CredentialResolver, ProviderCredential } from "./credentials.js"
export { DynamicModelCatalog, StaticModelCatalog } from "./model-catalog.js"
export type { ModelCatalog, ModelCatalogRefreshResult, ModelCatalogSource } from "./model-catalog.js"
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
