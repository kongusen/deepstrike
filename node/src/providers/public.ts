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
export type { OpenAIResponsesRunState } from "./openai-responses.js"
export { OpenAIChatAdapter } from "./openai-chat.js"
export { endpointProfiles, modelProfiles, getModelProfile } from "./profiles.js"
export type { ModelProfileId, ProviderId } from "./profiles.js"
export type { ProviderRunState, ProviderToolSpec, ProviderReplay, RenderedContext, CacheBreakpointStrategy } from "../types.js"
