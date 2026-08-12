import type { LLMProvider, RuntimePolicy } from "../types.js"
import {
  endpointProfiles,
  type EndpointProfile,
  type EndpointProfileId,
  type EndpointProtocol,
  type ProviderId,
} from "./endpoints.js"
import { GEMINI_PROTOCOL_CAPABILITIES } from "./gemini-adapter.js"

export type ModelKind = "generation" | "embedding"
export type InputModality = "text" | "image" | "audio" | "video" | "file"
export type OutputModality = "text" | "image" | "audio" | "embedding"
export type CapabilityState = "supported" | "unsupported" | "unknown"
export type GenerationProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "gemini"
  | "ollama-chat"

export const MODEL_CAPABILITY_STATES: readonly CapabilityState[] = [
  "supported",
  "unsupported",
  "unknown",
]

export interface ModelDescriptor {
  id: string
  providerId: string
  kind: ModelKind
  contextWindow?: number
  maxOutputTokens?: number
  intrinsic: {
    inputModalities?: readonly InputModality[]
    outputModalities?: readonly OutputModality[]
    tools?: boolean
    reasoning?: boolean
  }
}

export interface ModelRegistration {
  descriptor: ModelDescriptor
  defaultEndpointId: EndpointProfileId
  recommendedRuntimePolicy?: RuntimePolicy
}

export interface DynamicModelDescriptorResolver {
  readonly providerId: ProviderId
  resolve(modelId: string): ModelRegistration
}

export interface ProtocolRuntimeCapabilities {
  acceptedInputModalities: readonly InputModality[]
  emittedOutputModalities: readonly OutputModality[]
  tools: boolean
  parallelToolCalls?: boolean
  structuredOutput?: boolean
  reasoningReplay: "none" | "optional" | "required"
  promptCaching?: boolean
  mediaForms: {
    imageUrl?: boolean
    imageBase64?: boolean
    fileId?: boolean
    audioUrl?: boolean
    audioBase64?: boolean
  }
}

export interface ProtocolRuntimeCapabilityOverrides {
  acceptedInputModalities?: readonly InputModality[]
  emittedOutputModalities?: readonly OutputModality[]
  tools?: boolean
  parallelToolCalls?: boolean
  structuredOutput?: boolean
  reasoningReplay?: ProtocolRuntimeCapabilities["reasoningReplay"]
  promptCaching?: boolean
  mediaForms?: Partial<ProtocolRuntimeCapabilities["mediaForms"]>
}

export interface EndpointRuntimeCapabilities {
  nativeTokenCounting?: boolean
  protocolOverrides?: ProtocolRuntimeCapabilityOverrides
}

export type CapabilityEvidenceLayer = "model" | "protocol" | "endpoint"

export interface EffectiveCapability<T = boolean> {
  state: CapabilityState
  value?: T
  evidence: readonly CapabilityEvidenceLayer[]
}

export interface EffectiveModelCapabilities {
  inputModalities: Record<InputModality, EffectiveCapability>
  outputModalities: Record<OutputModality, EffectiveCapability>
  tools: EffectiveCapability
  reasoning: EffectiveCapability
  parallelToolCalls: EffectiveCapability
  structuredOutput: EffectiveCapability
  promptCaching: EffectiveCapability
  nativeTokenCounting: EffectiveCapability
  mediaForms: {
    imageUrl: EffectiveCapability
    imageBase64: EffectiveCapability
    fileId: EffectiveCapability
    audioUrl: EffectiveCapability
    audioBase64: EffectiveCapability
  }
}

export interface ResolvedProviderRuntime<TAdapter = LLMProvider> {
  identity: {
    providerId: ProviderId
    modelId: string
    endpointId: EndpointProfileId
    protocol: GenerationProtocol
  }
  model: ModelDescriptor
  endpoint: EndpointProfile
  adapter: TAdapter
  effectiveCapabilities: EffectiveModelCapabilities
  runtimePolicy?: RuntimePolicy
}

export interface RegistryRuleEvidence {
  ruleId: string
  classification: "routing" | "policy" | "protocol" | "endpoint"
  source: string
  verifiedAt: "2026-08-12"
}

export const registryEvidence: readonly RegistryRuleEvidence[] = [
  {
    ruleId: "provider-prefix-routing",
    classification: "routing",
    source: "node/tests/characterization/__golden__/model-facts-baseline.json",
    verifiedAt: "2026-08-12",
  },
  {
    ruleId: "runtime-policy-resolution",
    classification: "policy",
    source: "node/tests/characterization/__golden__/model-facts-baseline.json",
    verifiedAt: "2026-08-12",
  },
  {
    ruleId: "protocol-wire-capabilities",
    classification: "protocol",
    source: "node/tests/characterization/wire-request.test.ts",
    verifiedAt: "2026-08-12",
  },
  {
    ruleId: "native-token-counting",
    classification: "endpoint",
    source: "node/tests/anthropic-count-tokens.test.ts; node/tests/gemini-count-tokens.test.ts",
    verifiedAt: "2026-08-12",
  },
]

const POLICY: Record<string, RuntimePolicy> = {
  "anthropic/claude-opus-4-1": { maxTurns: 50 },
  "anthropic/claude-opus-4-7": { maxTurns: 50 },
  "anthropic/claude-opus-4-6": { maxTurns: 50 },
  "anthropic/claude-opus-4-0": { maxTurns: 50 },
  "anthropic/claude-sonnet-4-6": { maxTurns: 25 },
  "anthropic/claude-sonnet-4-0": { maxTurns: 25 },
  "anthropic/claude-haiku-4-5": { maxTurns: 15 },
  "anthropic/claude-3-5-haiku-latest": { maxTurns: 15 },
  "deepseek/deepseek-chat": { maxTurns: 25 },
  "deepseek/deepseek-reasoner": { maxTurns: 50 },
  "deepseek/deepseek-v4-flash": { maxTurns: 20 },
  "deepseek/deepseek-v4-pro": { maxTurns: 35 },
  "kimi/moonshot-v1-8k": { maxTurns: 15 },
  "kimi/moonshot-v1-32k": { maxTurns: 20 },
  "kimi/moonshot-v1-128k": { maxTurns: 30 },
  "kimi/kimi-k2.5": { maxTurns: 30 },
  "kimi/kimi-k2.6": { maxTurns: 35 },
  "kimi/kimi-k2-thinking": { maxTurns: 50 },
  "kimi/kimi-k2-thinking-turbo": { maxTurns: 40 },
  "qwen/qwen3.7-max-preview": { maxTurns: 45 },
  "qwen/qwen3.7-plus-preview": { maxTurns: 40 },
  "qwen/qwen3.6-max-preview": { maxTurns: 40 },
  "qwen/qwen3.6-plus": { maxTurns: 35 },
  "qwen/qwen3.6-flash": { maxTurns: 20 },
  "qwen/qwen3.6-35b-a3b": { maxTurns: 25 },
  "qwen/qwen3.6-27b": { maxTurns: 25 },
  "qwen/qwen3.5-plus": { maxTurns: 35 },
  "qwen/qwen3.5-flash": { maxTurns: 20 },
  "qwen/qwen3.5-397b-a17b": { maxTurns: 35 },
  "qwen/qwen3.5-122b-a10b": { maxTurns: 25 },
  "qwen/qwen3.5-35b-a3b": { maxTurns: 20 },
  "qwen/qwen3.5-27b": { maxTurns: 20 },
  "glm/glm-5.2": { maxTurns: 50 },
  "glm/glm-5.1": { maxTurns: 50 },
  "glm/glm-4-plus": { maxTurns: 35 },
  "glm/glm-4-flash": { maxTurns: 15 },
  "glm/glm-4-air": { maxTurns: 20 },
  "minimax/MiniMax-M3": { maxTurns: 35 },
  "minimax/MiniMax-M3-highspeed": { maxTurns: 35 },
  "minimax/MiniMax-M2.7": { maxTurns: 35 },
  "minimax/MiniMax-M2.7-highspeed": { maxTurns: 35 },
  "minimax/MiniMax-M2.5": { maxTurns: 25 },
  "minimax/MiniMax-M2.5-highspeed": { maxTurns: 25 },
  "minimax/MiniMax-M2.1": { maxTurns: 25 },
  "minimax/MiniMax-M2.1-highspeed": { maxTurns: 25 },
  "minimax/MiniMax-M2": { maxTurns: 20 },
  "minimax/MiniMax-Text-01": { maxTurns: 20 },
  "gemini/gemini-3-pro-preview": { maxTurns: 50 },
  "gemini/gemini-3-flash-preview": { maxTurns: 25 },
  "gemini/gemini-3.5-flash": { maxTurns: 30 },
  "gemini/gemini-2.5-pro": { maxTurns: 35 },
  "gemini/gemini-2.5-flash": { maxTurns: 20 },
  "gemini/gemini-2.0-flash": { maxTurns: 15 },
  "gemini/gemini-2.0-flash-lite": { maxTurns: 10 },
  "gemini/gemini-1.5-pro": { maxTurns: 30 },
  "gemini/gemini-1.5-flash": { maxTurns: 15 },
  "openai/gpt-5.5": { maxTurns: 60 },
  "openai/gpt-5.4": { maxTurns: 50 },
  "openai/gpt-5.4-mini": { maxTurns: 25 },
  "openai/gpt-5.4-nano": { maxTurns: 15 },
  "openai/gpt-5.2": { maxTurns: 50 },
  "openai/gpt-5.2-pro": { maxTurns: 60 },
  "openai/gpt-5.1": { maxTurns: 50 },
  "openai/gpt-4o": { maxTurns: 25 },
  "openai/gpt-4o-mini": { maxTurns: 15 },
  "openai/gpt-4.1": { maxTurns: 35 },
  "openai/gpt-4.1-mini": { maxTurns: 20 },
  "openai/gpt-4.1-nano": { maxTurns: 15 },
  "openai/gpt-5": { maxTurns: 50 },
  "openai/gpt-5-pro": { maxTurns: 60 },
  "openai/gpt-5-mini": { maxTurns: 25 },
  "openai/gpt-5-nano": { maxTurns: 15 },
  "openai/o1": { maxTurns: 50 },
  "openai/o1-mini": { maxTurns: 25 },
  "openai/o3": { maxTurns: 50 },
  "openai/o3-mini": { maxTurns: 25 },
  "openai/o4-mini": { maxTurns: 25 },
}

const DEFAULT_ENDPOINT: Record<ProviderId, EndpointProfileId> = {
  anthropic: "anthropic.messages",
  openai: "openai.chat",
  minimax: "minimax.anthropic",
  deepseek: "deepseek.anthropic",
  kimi: "kimi.anthropic",
  qwen: "qwen.anthropic",
  gemini: "gemini.google",
  glm: "glm.anthropic",
  baai: "baai.self-hosted.embeddings",
  ollama: "ollama.local",
}

const DEFAULT_MODEL: Record<ProviderId, string> = {
  anthropic: "claude-sonnet-4-6",
  openai: "gpt-4o",
  minimax: "MiniMax-M3",
  deepseek: "deepseek-v4-flash",
  kimi: "kimi-k2.6",
  qwen: "qwen3.6-plus",
  gemini: "gemini-2.0-flash",
  glm: "glm-5.2",
  baai: "bge-m3",
  ollama: "llama3",
}

export function defaultModelForProvider(providerId: ProviderId): string {
  return DEFAULT_MODEL[providerId]
}

function endpointFor(providerId: ProviderId, modelId: string): EndpointProfileId {
  if (providerId === "openai") {
    if (modelId.startsWith("text-embedding-")) return "openai.embeddings"
    if (/^(gpt-5|gpt-4\.1|o3|o4-mini)/.test(modelId)) return "openai.responses"
  }
  if (providerId === "qwen") {
    if (modelId.startsWith("text-embedding-")) return "qwen.dashscope.embeddings"
    if (/^(qwen2\.5-vl-embedding|qwen3-vl-embedding)$/.test(modelId)) {
      return "qwen.dashscope.multimodal-embeddings"
    }
  }
  if (providerId === "gemini" && modelId.startsWith("gemini-embedding-")) {
    return "gemini.google.embeddings"
  }
  if (providerId === "glm" && modelId.startsWith("embedding-")) {
    return "glm.openai.embeddings"
  }
  return DEFAULT_ENDPOINT[providerId]
}

function modelKind(endpointId: EndpointProfileId): ModelKind {
  return generationProtocol(endpointProfiles[endpointId].protocol) ? "generation" : "embedding"
}

function registration(providerId: ProviderId, rawModelId: string): ModelRegistration {
  const modelId = normalizeModelId(providerId, rawModelId)
  const endpointId = endpointFor(providerId, modelId)
  const policy = providerId === "ollama" ? ollamaPolicy(modelId) : POLICY[`${providerId}/${modelId}`]
  return {
    descriptor: {
      id: `${providerId}/${modelId}`,
      providerId,
      kind: modelKind(endpointId),
      intrinsic: {},
    },
    defaultEndpointId: endpointId,
    ...(policy ? { recommendedRuntimePolicy: policy } : {}),
  }
}

const dynamicResolvers = Object.fromEntries(
  (Object.keys(DEFAULT_ENDPOINT) as ProviderId[]).map(providerId => [providerId, {
    providerId,
    resolve(modelId: string) {
      return registration(providerId, modelId)
    },
  } satisfies DynamicModelDescriptorResolver]),
) as Record<ProviderId, DynamicModelDescriptorResolver>

export class ModelRegistry {
  resolve(modelId: string, providerId?: ProviderId): ModelRegistration | undefined {
    const resolvedProvider = providerId ?? providerPrefix(modelId)
    if (!resolvedProvider) return undefined
    return dynamicResolvers[resolvedProvider].resolve(modelId)
  }
}

export const modelRegistry = new ModelRegistry()

export function normalizeModelId(providerId: string, modelId: string): string {
  const prefix = `${providerId}/`
  return modelId.startsWith(prefix) ? modelId.slice(prefix.length) : modelId
}

export function getRuntimePolicy(providerId: string, modelId: string): RuntimePolicy {
  if (!isKnownProviderId(providerId)) return {}
  return modelRegistry.resolve(modelId, providerId)?.recommendedRuntimePolicy ?? {}
}

function providerPrefix(modelId: string): ProviderId | undefined {
  const slash = modelId.indexOf("/")
  if (slash <= 0) return undefined
  const value = modelId.slice(0, slash)
  return isKnownProviderId(value) ? value : undefined
}

function ollamaPolicy(modelId: string): RuntimePolicy {
  const lower = modelId.toLowerCase()
  const rows: ReadonlyArray<readonly [string, number]> = [
    ["deepseek-r1", 40], ["qwq", 35], ["llama3.3", 25], ["llama3.2", 20],
    ["llama3.1", 20], ["llama3", 20], ["mistral", 20], ["gemma2", 20],
    ["phi4", 20], ["phi3", 15], ["codellama", 20],
  ]
  return { maxTurns: rows.find(([prefix]) => lower.startsWith(prefix))?.[1] ?? 20 }
}

export const protocolRuntimeCapabilities: Record<GenerationProtocol, ProtocolRuntimeCapabilities> = {
  "anthropic-messages": {
    acceptedInputModalities: ["text", "image"], emittedOutputModalities: ["text"], tools: true,
    parallelToolCalls: true, structuredOutput: false, reasoningReplay: "required", promptCaching: true,
    mediaForms: { imageUrl: true, imageBase64: true },
  },
  "openai-chat": {
    acceptedInputModalities: ["text", "image", "audio"], emittedOutputModalities: ["text"], tools: true,
    parallelToolCalls: true, structuredOutput: true, reasoningReplay: "optional", promptCaching: true,
    mediaForms: { imageUrl: true, imageBase64: true, audioBase64: true },
  },
  "openai-responses": {
    acceptedInputModalities: ["text", "image", "file"], emittedOutputModalities: ["text"], tools: true,
    parallelToolCalls: true, structuredOutput: true, reasoningReplay: "optional", promptCaching: true,
    mediaForms: { imageUrl: true, imageBase64: true, fileId: true },
  },
  gemini: GEMINI_PROTOCOL_CAPABILITIES,
  "ollama-chat": {
    acceptedInputModalities: ["text", "image"], emittedOutputModalities: ["text"], tools: true,
    reasoningReplay: "none", mediaForms: { imageBase64: true },
  },
}

export const endpointRuntimeCapabilities: Partial<Record<EndpointProfileId, EndpointRuntimeCapabilities>> = {
  "anthropic.messages": { nativeTokenCounting: true },
  "gemini.google": { nativeTokenCounting: true },
}

export function resolveEffectiveCapability<T = boolean>(
  layers: readonly { layer: CapabilityEvidenceLayer; state: CapabilityState; value?: T }[],
): EffectiveCapability<T> {
  const evidence = layers.filter(layer => layer.state !== "unknown").map(layer => layer.layer)
  if (layers.some(layer => layer.state === "unsupported")) return { state: "unsupported", evidence }
  if (layers.length > 0 && layers.every(layer => layer.state === "supported")) {
    const valued = [...layers].reverse().find(layer => layer.value !== undefined)
    return { state: "supported", ...(valued ? { value: valued.value } : {}), evidence }
  }
  return { state: "unknown", evidence }
}

const inputModalities: readonly InputModality[] = ["text", "image", "audio", "video", "file"]
const outputModalities: readonly OutputModality[] = ["text", "image", "audio", "embedding"]

function booleanState(value: boolean | undefined): CapabilityState {
  return value === undefined ? "unknown" : value ? "supported" : "unsupported"
}

function membershipState(values: readonly string[] | undefined, value: string): CapabilityState {
  return values === undefined ? "unknown" : values.includes(value) ? "supported" : "unsupported"
}

export function resolveEffectiveModelCapabilities(input: {
  model: ModelDescriptor
  protocol: GenerationProtocol
  endpointCapabilities?: EndpointRuntimeCapabilities
}): EffectiveModelCapabilities {
  const protocol = protocolRuntimeCapabilities[input.protocol]
  const overrides = input.endpointCapabilities?.protocolOverrides
  const modality = (layer: "input" | "output", value: InputModality | OutputModality) => {
    const modelValues = layer === "input"
      ? input.model.intrinsic.inputModalities
      : input.model.intrinsic.outputModalities
    const protocolValues = layer === "input"
      ? protocol.acceptedInputModalities
      : protocol.emittedOutputModalities
    const overrideValues = layer === "input"
      ? overrides?.acceptedInputModalities
      : overrides?.emittedOutputModalities
    return resolveEffectiveCapability([
      { layer: "model", state: membershipState(modelValues, value) },
      { layer: "protocol", state: membershipState(protocolValues, value) },
      ...(overrideValues ? [{ layer: "endpoint" as const, state: membershipState(overrideValues, value) }] : []),
    ])
  }
  const booleanCapability = (
    modelValue: boolean | undefined,
    protocolValue: boolean | undefined,
    endpointValue?: boolean,
  ) => resolveEffectiveCapability([
    { layer: "model", state: booleanState(modelValue) },
    { layer: "protocol", state: booleanState(protocolValue) },
    ...(endpointValue === undefined ? [] : [{ layer: "endpoint" as const, state: booleanState(endpointValue) }]),
  ])
  const protocolBoolean = (protocolValue: boolean | undefined, endpointValue?: boolean) => resolveEffectiveCapability([
    { layer: "protocol", state: booleanState(protocolValue) },
    ...(endpointValue === undefined ? [] : [{ layer: "endpoint" as const, state: booleanState(endpointValue) }]),
  ])
  const media = (key: keyof ProtocolRuntimeCapabilities["mediaForms"]) => protocolBoolean(
    protocol.mediaForms[key],
    overrides?.mediaForms?.[key],
  )

  return {
    inputModalities: Object.fromEntries(inputModalities.map(value => [value, modality("input", value)])) as Record<InputModality, EffectiveCapability>,
    outputModalities: Object.fromEntries(outputModalities.map(value => [value, modality("output", value)])) as Record<OutputModality, EffectiveCapability>,
    tools: booleanCapability(input.model.intrinsic.tools, protocol.tools, overrides?.tools),
    reasoning: resolveEffectiveCapability([
      { layer: "model", state: booleanState(input.model.intrinsic.reasoning) },
      { layer: "protocol", state: protocol.reasoningReplay === "none" ? "unknown" : "supported" },
    ]),
    parallelToolCalls: protocolBoolean(protocol.parallelToolCalls, overrides?.parallelToolCalls),
    structuredOutput: protocolBoolean(protocol.structuredOutput, overrides?.structuredOutput),
    promptCaching: protocolBoolean(protocol.promptCaching, overrides?.promptCaching),
    nativeTokenCounting: resolveEffectiveCapability([
      { layer: "endpoint", state: booleanState(input.endpointCapabilities?.nativeTokenCounting) },
    ]),
    mediaForms: {
      imageUrl: media("imageUrl"), imageBase64: media("imageBase64"), fileId: media("fileId"),
      audioUrl: media("audioUrl"), audioBase64: media("audioBase64"),
    },
  }
}

export function generationProtocol(protocol: EndpointProtocol): GenerationProtocol | undefined {
  switch (protocol) {
    case "anthropic-messages":
    case "openai-chat":
    case "openai-responses":
    case "gemini":
    case "ollama-chat":
      return protocol
    default:
      return undefined
  }
}

export function endpointCapabilitiesFor(
  endpointId: EndpointProfileId,
  preserveEndpointIdentity: boolean,
): EndpointRuntimeCapabilities | undefined {
  return preserveEndpointIdentity ? endpointRuntimeCapabilities[endpointId] : undefined
}

export function isKnownProviderId(value: string): value is ProviderId {
  return value in DEFAULT_ENDPOINT
}
