import type { RenderedContext, ToolSchema } from "../types.js"
import { sha256Hex } from "../runtime/sha256.js"

export interface ProviderRequestEndpoint { id: string; protocol: string; baseURL: string }
export interface ProviderRequestPlan {
  providerId: string
  modelId: string
  endpoint: ProviderRequestEndpoint
  context: RenderedContext
  tools: ToolSchema[]
  options: Record<string, unknown>
  fingerprint: string
}

/** Stable, provider-independent fallback used when no native token meter is available. */
export function estimateProviderPromptTokens(context: RenderedContext, tools: ToolSchema[]): number {
  const bytes = new TextEncoder().encode(stableJson({ context, tools })).byteLength
  return Math.max(1, Math.ceil(bytes / 4))
}
export interface RecordedPromptMeasurement {
  requestFingerprint: string
  inputTokens: number
  source: { kind: "native"; provider: string } | { kind: "local_exact"; tokenizer: string } | { kind: "heuristic" }
  confidence: "exact" | "high_confidence" | "low_confidence"
}

export interface ProviderUsage {
  inputTokens: number
  outputTokens: number
  cacheReadInputTokens?: number
  cacheCreationInputTokens?: number
  reasoningTokens?: number
}

export interface NormalizedProviderUsage extends ProviderUsage {
  uncachedInputTokens: number
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

const EXCLUDED = new Set(["apiKey", "api_key", "bearerToken", "bearer_token", "authorization", "credential", "credentials", "retry", "maxRetries", "baseDelay", "timeout", "signal"])

export function createProviderRequestPlan(input: Omit<ProviderRequestPlan, "fingerprint" | "options"> & { options?: Record<string, unknown> }): ProviderRequestPlan {
  const value = {
    providerId: input.providerId, modelId: input.modelId, endpoint: sanitizeEndpoint(input.endpoint),
    context: clone(input.context), tools: clone(input.tools), options: materialOptions(input.options ?? {}),
  }
  return { ...value, fingerprint: sha256Hex(stableJson(value)) }
}

export function createProviderRequestPlanForProvider(
  provider: {
    descriptor?(): { provider: string; protocol: string; model: string }
    requestPlanIdentity?(): { providerId?: string; modelId?: string; endpoint?: { id?: string; protocol?: string; baseURL?: string } }
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
    context, tools, options,
  })
}

export function recordPromptMeasurement(plan: Pick<ProviderRequestPlan, "fingerprint">, measurement: Omit<RecordedPromptMeasurement, "requestFingerprint">): RecordedPromptMeasurement {
  if (!Number.isSafeInteger(measurement.inputTokens) || measurement.inputTokens < 0) throw new RangeError("inputTokens must be a non-negative safe integer")
  return { requestFingerprint: plan.fingerprint, inputTokens: measurement.inputTokens, source: clone(measurement.source), confidence: measurement.confidence }
}

export function measurementForPlan(plan: Pick<ProviderRequestPlan, "fingerprint">, record: RecordedPromptMeasurement | undefined): RecordedPromptMeasurement | undefined {
  if (!record || record.requestFingerprint !== plan.fingerprint) return undefined
  if (!Number.isSafeInteger(record.inputTokens) || record.inputTokens < 0) return undefined
  if (record.confidence !== "exact" && record.confidence !== "high_confidence" && record.confidence !== "low_confidence") return undefined
  if (!record.source || typeof record.source !== "object") return undefined
  if (record.source.kind === "native" && typeof record.source.provider === "string" && record.source.provider.length > 0) return clone(record)
  if (record.source.kind === "local_exact" && typeof record.source.tokenizer === "string" && record.source.tokenizer.length > 0) return clone(record)
  if (record.source.kind === "heuristic") return clone(record)
  return undefined
}

export function normalizeProviderUsage(usage: ProviderUsage): NormalizedProviderUsage {
  const inputTokens = nonNegativeInteger(usage.inputTokens, "inputTokens")
  const outputTokens = nonNegativeInteger(usage.outputTokens, "outputTokens")
  const cacheReadInputTokens = optionalNonNegativeInteger(usage.cacheReadInputTokens, "cacheReadInputTokens")
  const cacheCreationInputTokens = optionalNonNegativeInteger(usage.cacheCreationInputTokens, "cacheCreationInputTokens")
  const reasoningTokens = optionalNonNegativeInteger(usage.reasoningTokens, "reasoningTokens")
  const cached = (cacheReadInputTokens ?? 0) + (cacheCreationInputTokens ?? 0)
  if (cached > inputTokens) throw new RangeError("cache token subsets cannot exceed inputTokens")
  if (reasoningTokens !== undefined && reasoningTokens > outputTokens) throw new RangeError("reasoningTokens must be a subset of outputTokens")
  return {
    inputTokens,
    uncachedInputTokens: inputTokens - cached,
    outputTokens,
    ...(cacheReadInputTokens !== undefined ? { cacheReadInputTokens } : {}),
    ...(cacheCreationInputTokens !== undefined ? { cacheCreationInputTokens } : {}),
    ...(reasoningTokens !== undefined ? { reasoningTokens } : {}),
  }
}

export function priceProviderUsage(usage: NormalizedProviderUsage, snapshot: PricingSnapshot, observedAt: string | Date = new Date()): CostObservation {
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
  if (expires && (Number.isNaN(expires.valueOf()) || at >= expires)) return { source: "unpriced", reason: "pricing_snapshot_expired" }
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
      if (EXCLUDED.has(key) || isTransportOnlyKey(key)) return []
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

function clone<T>(value: T): T {
  if (value === undefined || value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone) as T
  return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, item]) => [key, clone(item)])) as T
}

function nonNegativeInteger(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value < 0) throw new RangeError(`${name} must be a non-negative safe integer`)
  return value
}

function optionalNonNegativeInteger(value: number | undefined, name: string): number | undefined {
  return value === undefined ? undefined : nonNegativeInteger(value, name)
}
