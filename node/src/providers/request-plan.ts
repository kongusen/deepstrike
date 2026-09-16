import { createHash } from "node:crypto"
import { createRequire } from "node:module"
import type { RenderedContext, ToolSchema, ProviderUsage } from "../types.js"
import type { GenerationProtocol } from "./protocol-capabilities.js"

export interface ProviderRequestEndpoint {
  id: string
  protocol: string
  baseURL: string
}

/** One provider-visible request, deliberately excluding credentials and transport-only retries. */
export interface ProviderRequestPlan {
  providerId: string
  modelId: string
  endpoint: ProviderRequestEndpoint
  context: RenderedContext
  tools: ToolSchema[]
  options: Record<string, unknown>
  fingerprint: string
  stablePrefixFingerprint: string
}

export interface NormalizedProviderUsage extends ProviderUsage {
  /** Input not accounted as a cache read or write. `inputTokens` remains the full footprint. */
  uncachedInputTokens: number
}

/** Durable host fact: a specific logical request was counted before execution. */
export interface RecordedPromptMeasurement {
  requestFingerprint: string
  inputTokens: number
  source:
    | { kind: "native"; provider: string }
    | { kind: "local_exact"; tokenizer: string }
    | { kind: "postflight" }
    | { kind: "heuristic" }
  confidence: "exact" | "high_confidence" | "low_confidence"
}

export interface PricingSnapshot {
  version: string
  currency: string
  region: string
  effectiveFrom: string
  expiresAt?: string
  ratesPerMillion: {
    input: number
    output: number
    cacheRead?: number
    cacheCreation?: number
    reasoning?: number
  }
}

export type CostObservation =
  | { source: "snapshot"; currency: string; amount: number; pricingVersion: string }
  | { source: "unpriced"; reason: "pricing_snapshot_not_effective" | "pricing_snapshot_expired" | "invalid_pricing_snapshot" }

const TRANSPORT_ONLY_KEYS = new Set([
  "apiKey", "api_key", "bearerToken", "bearer_token", "authorization", "credential",
  "credentials", "retry", "maxRetries", "baseDelay", "timeout", "signal", "access_token", "refresh_token", "token", "secret", "x-api-key",
])

export function createProviderRequestPlan(input: Omit<ProviderRequestPlan, "fingerprint" | "stablePrefixFingerprint" | "options"> & {
  options?: Record<string, unknown>
}): ProviderRequestPlan {
  const options = materialOptions(input.options ?? {})
  const plan = {
    providerId: input.providerId,
    modelId: input.modelId,
    endpoint: sanitizeEndpoint(input.endpoint),
    context: clone(input.context),
    tools: clone(input.tools),
    options,
  }
  const stablePrefix = {
    providerId: plan.providerId,
    modelId: plan.modelId,
    endpoint: plan.endpoint,
    context: stablePrefixContext(plan.context),
    tools: plan.tools,
    options: plan.options,
  }
  return {
    ...plan,
    fingerprint: sha256(stableJson(plan)),
    stablePrefixFingerprint: sha256(stableJson(stablePrefix)),
  }
}

/** Build the plan from a resolved provider when the runner only has the public provider object. */
export function createProviderRequestPlanForProvider(
  provider: {
    descriptor?(): { provider: string; protocol: string; model: string }
    requestPlanIdentity?(): {
      providerId?: string
      modelId?: string
      endpoint?: { id?: string; protocol?: string; baseURL?: string }
    }
  },
  context: RenderedContext,
  tools: ToolSchema[],
  options?: Record<string, unknown>,
): ProviderRequestPlan {
  const descriptor = provider.descriptor?.() ?? { provider: "unknown", protocol: "unknown", model: "unknown" }
  const identity = provider.requestPlanIdentity?.()
  return createProviderRequestPlan({
    providerId: identity?.providerId ?? descriptor.provider,
    modelId: identity?.modelId ?? descriptor.model,
    endpoint: {
      id: identity?.endpoint?.id ?? `${descriptor.provider}.${descriptor.protocol}`,
      protocol: identity?.endpoint?.protocol ?? descriptor.protocol,
      baseURL: identity?.endpoint?.baseURL ?? "",
    },
    context,
    tools,
    options,
  })
}

/**
 * P4 §1.3: the resolved route one provider execution is pinned to. Content-addressed
 * (`routeId` = stable hash of every field except `capabilitiesRef`); the capability table's
 * authority stays the static vocabulary in protocol-capabilities.ts — this holds a reference,
 * never a copy.
 */
export interface ResolvedProviderRoute {
  routeId: string
  provider: string
  protocol: GenerationProtocol
  model: string
  endpoint: ProviderRequestEndpoint
  /** Adapter implementation version (the @deepstrike/sdk package version; F8). */
  adapterVersion: string
  /** Capability snapshot reference: the protocol constant name (e.g. "anthropic-messages").
   *  A provider exposing capability overrides would append a summary here — none does today. */
  capabilitiesRef: string
}

const KNOWN_GENERATION_PROTOCOLS = new Set<GenerationProtocol>([
  "anthropic-messages",
  "openai-chat",
  "openai-responses",
  "gemini",
  "ollama-chat",
])

/**
 * P4-S1: assemble the run's route once at runner construction (P4 §0.2 — the provider is
 * fixed for the run today; a future failover resolver re-evaluates per attempt with the same
 * shape). Never throws: evidence assembly degrades to "unknown" fields rather than breaking a run.
 */
export function resolveProviderRoute(provider: {
  descriptor?(): { provider: string; protocol: string; model: string }
  requestPlanIdentity?(): {
    providerId?: string
    modelId?: string
    endpoint?: { id?: string; protocol?: string; baseURL?: string }
  }
}): ResolvedProviderRoute {
  try {
    const descriptor = provider.descriptor?.() ?? { provider: "unknown", protocol: "unknown", model: "unknown" }
    const identity = provider.requestPlanIdentity?.()
    const protocolRaw = identity?.endpoint?.protocol ?? descriptor.protocol
    // Evidence records what the descriptor said, even when a foreign provider speaks a protocol
    // outside the in-tree vocabulary — the field is a report, not a gate.
    const protocol = protocolRaw as GenerationProtocol
    const route = {
      provider: identity?.providerId ?? descriptor.provider,
      protocol,
      model: identity?.modelId ?? descriptor.model,
      endpoint: sanitizeEndpoint({
        id: identity?.endpoint?.id ?? `${descriptor.provider}.${descriptor.protocol}`,
        protocol: protocolRaw,
        baseURL: identity?.endpoint?.baseURL ?? "",
      }),
      adapterVersion: adapterPackageVersion(),
      capabilitiesRef: KNOWN_GENERATION_PROTOCOLS.has(protocol) ? protocol : "unknown",
    }
    // §1.3: content addressing covers every field EXCEPT capabilitiesRef (a reference, not content).
    const { capabilitiesRef: _ref, ...addressed } = route
    return { ...route, routeId: sha256(stableJson(addressed)) }
  } catch {
    const fallback = {
      provider: "unknown",
      protocol: "unknown" as GenerationProtocol,
      model: "unknown",
      endpoint: { id: "unknown", protocol: "unknown", baseURL: "" },
      adapterVersion: "unknown",
    }
    return { ...fallback, capabilitiesRef: "unknown", routeId: sha256(stableJson(fallback)) }
  }
}

/** The adapter's package version, read from package.json at runtime. "unknown" when unreadable —
 *  route assembly is evidence plumbing and must never throw (B7: evidence, not kernel input). */
function adapterPackageVersion(): string {
  try {
    // From both src/providers/ (ts) and dist/providers/ (js), ../../package.json is the SDK manifest.
    const require = createRequire(import.meta.url)
    const manifest = require("../../package.json") as { version?: unknown }
    return typeof manifest.version === "string" && manifest.version.length > 0 ? manifest.version : "unknown"
  } catch {
    return "unknown"
  }
}

export function estimateProviderPromptTokens(context: RenderedContext, tools: ToolSchema[]): number {
  const bytes = new TextEncoder().encode(stableJson({ context, tools })).byteLength
  return Math.max(1, Math.ceil(bytes / 4))
}

/** Bind a preflight count to its exact provider-visible request. Replay only reuses matching facts. */
export function recordPromptMeasurement(
  plan: Pick<ProviderRequestPlan, "fingerprint">,
  measurement: Omit<RecordedPromptMeasurement, "requestFingerprint">,
): RecordedPromptMeasurement {
  return {
    requestFingerprint: plan.fingerprint,
    inputTokens: requireNonNegativeInteger(measurement.inputTokens, "inputTokens"),
    source: clone(measurement.source),
    confidence: measurement.confidence,
  }
}

export function measurementForPlan(
  plan: Pick<ProviderRequestPlan, "fingerprint">,
  recorded: RecordedPromptMeasurement | undefined,
): RecordedPromptMeasurement | undefined {
  if (!recorded || recorded.requestFingerprint !== plan.fingerprint) return undefined
  if (!Number.isSafeInteger(recorded.inputTokens) || recorded.inputTokens < 0) return undefined
  if (recorded.confidence !== "exact" && recorded.confidence !== "high_confidence" && recorded.confidence !== "low_confidence") return undefined
  const source = recorded.source
  if (!source || typeof source !== "object") return undefined
  if (source.kind === "native" && typeof source["provider"] === "string" && source["provider"].length > 0) return clone(recorded)
  if (source.kind === "local_exact" && typeof source.tokenizer === "string" && source.tokenizer.length > 0) return clone(recorded)
  if (source.kind === "postflight") return clone(recorded)
  if (source.kind === "heuristic") return clone(recorded)
  return undefined
}

/** Normalize postflight provider facts without turning estimates into actual usage. */
export function normalizeProviderUsage(usage: ProviderUsage): NormalizedProviderUsage {
  const inputTokens = requireNonNegativeInteger(usage.inputTokens, "inputTokens")
  const outputTokens = requireNonNegativeInteger(usage.outputTokens, "outputTokens")
  const cacheReadInputTokens = optionalNonNegativeInteger(usage.cacheReadInputTokens, "cacheReadInputTokens")
  const cacheCreationInputTokens = optionalNonNegativeInteger(usage.cacheCreationInputTokens, "cacheCreationInputTokens")
  const reasoningTokens = optionalNonNegativeInteger(usage.reasoningTokens, "reasoningTokens")
  const cached = (cacheReadInputTokens ?? 0) + (cacheCreationInputTokens ?? 0)
  if (cached > inputTokens) throw new RangeError("cache token subsets cannot exceed inputTokens")
  if (reasoningTokens !== undefined && reasoningTokens > outputTokens) {
    throw new RangeError("reasoningTokens must be a subset of outputTokens")
  }
  return {
    inputTokens,
    uncachedInputTokens: inputTokens - cached,
    outputTokens,
    ...(cacheReadInputTokens !== undefined ? { cacheReadInputTokens } : {}),
    ...(cacheCreationInputTokens !== undefined ? { cacheCreationInputTokens } : {}),
    ...(reasoningTokens !== undefined ? { reasoningTokens } : {}),
  }
}

/** Cost is derived only from an explicit, time-valid host snapshot; otherwise it stays unknown. */
export function priceProviderUsage(
  usage: NormalizedProviderUsage,
  snapshot: PricingSnapshot,
  observedAt: string | Date = new Date(),
): CostObservation {
  const at = typeof observedAt === "string" ? new Date(observedAt) : observedAt
  const from = new Date(snapshot.effectiveFrom)
  const expires = snapshot.expiresAt ? new Date(snapshot.expiresAt) : undefined
  const rates = snapshot.ratesPerMillion
  const requiredRates = [rates.input, rates.output]
  if (!snapshot.version || !snapshot.currency || Number.isNaN(at.valueOf()) || Number.isNaN(from.valueOf())
    || requiredRates.some(rate => typeof rate !== "number" || !Number.isFinite(rate) || rate < 0)
    || Object.values(rates).some(rate => typeof rate !== "number" || !Number.isFinite(rate) || rate < 0)) {
    return { source: "unpriced", reason: "invalid_pricing_snapshot" }
  }
  if (at < from) return { source: "unpriced", reason: "pricing_snapshot_not_effective" }
  if (expires && (Number.isNaN(expires.valueOf()) || at >= expires)) {
    return { source: "unpriced", reason: "pricing_snapshot_expired" }
  }
  const amount = (
    usage.uncachedInputTokens * rates.input
    + usage.outputTokens * rates.output
    + (usage.cacheReadInputTokens ?? 0) * (rates.cacheRead ?? rates.input)
    + (usage.cacheCreationInputTokens ?? 0) * (rates.cacheCreation ?? rates.input)
    + (usage.reasoningTokens ?? 0) * (rates.reasoning ?? 0)
  ) / 1_000_000
  return { source: "snapshot", currency: snapshot.currency, amount, pricingVersion: snapshot.version }
}

function materialOptions(options: Record<string, unknown>): Record<string, unknown> {
  return sanitizeMaterialValue(options) as Record<string, unknown>
}

function sanitizeEndpoint(endpoint: ProviderRequestEndpoint): ProviderRequestEndpoint {
  try {
    const url = new URL(endpoint.baseURL)
    url.username = ""
    url.password = ""
    url.search = ""
    url.hash = ""
    return { ...clone(endpoint), baseURL: url.toString().replace(/\/$/, "") }
  } catch {
    return { ...clone(endpoint), baseURL: "" }
  }
}

function sanitizeMaterialValue(value: unknown): unknown {
  if (value === undefined || typeof value === "function") return undefined
  if (Array.isArray(value)) return value.map(sanitizeMaterialValue).filter(item => item !== undefined)
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.keys(value as Record<string, unknown>).sort().flatMap(key => {
      if (TRANSPORT_ONLY_KEYS.has(key) || isTransportOnlyKey(key)) return []
      const sanitized = sanitizeMaterialValue((value as Record<string, unknown>)[key])
      return sanitized === undefined ? [] : [[key, sanitized]]
    }))
  }
  return value
}

function isTransportOnlyKey(key: string): boolean {
  const normalized = key.toLowerCase().replace(/[^a-z0-9]/g, "")
  return normalized.includes("authorization") || normalized.includes("credential")
    || normalized.includes("accesstoken") || normalized.includes("refreshtoken")
    || normalized.includes("apikey") || normalized === "bearer" || normalized === "token"
    || normalized === "secret" || normalized === "xapikey"
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`
  const object = value as Record<string, unknown>
  return `{${Object.keys(object).sort().map(key => `${JSON.stringify(key)}:${stableJson(object[key])}`).join(",")}}`
}

function stablePrefixContext(context: RenderedContext): Record<string, unknown> {
  const frozenPrefixLen = context.frozenPrefixLen ?? 0
  return {
    systemText: context.systemText,
    ...(context.systemStable !== undefined ? { systemStable: context.systemStable } : {}),
    ...(context.systemKnowledge !== undefined ? { systemKnowledge: context.systemKnowledge } : {}),
    frozenPrefixLen,
    turns: clone(context.turns.slice(0, frozenPrefixLen)),
  }
}

function sha256(value: string): string {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`
}

function clone<T>(value: T): T {
  if (value === undefined || value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone) as T
  return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, item]) => [key, clone(item)])) as T
}

function requireNonNegativeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new RangeError(`${name} must be a non-negative safe integer`)
  return value
}

function optionalNonNegativeInteger(value: number | undefined, name: string): number | undefined {
  return value === undefined ? undefined : requireNonNegativeInteger(value, name)
}
