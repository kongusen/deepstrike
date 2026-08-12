import { createHash } from "node:crypto"
import type { RenderedContext, ToolSchema, ProviderUsage } from "../types.js"

export interface ProviderRequestEndpoint {
  id: string
  protocol: string
  baseURL: string
}

/** One provider-visible request, deliberately excluding credentials and transport-only retries. */
export interface ProviderRequestPlan {
  version: 1
  providerId: string
  modelId: string
  endpoint: ProviderRequestEndpoint
  context: RenderedContext
  tools: ToolSchema[]
  options: Record<string, unknown>
  fingerprint: string
}

export interface NormalizedProviderUsage extends ProviderUsage {
  /** Input not accounted as a cache read or write. `inputTokens` remains the full footprint. */
  uncachedInputTokens: number
}

/** Durable host fact: a specific logical request was counted before execution. */
export interface RecordedPromptMeasurement {
  version: 1
  requestFingerprint: string
  inputTokens: number
  source:
    | { kind: "native"; provider: string }
    | { kind: "local_exact"; tokenizer: string }
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
  "credentials", "retry", "maxRetries", "baseDelay", "timeout", "signal",
])

export function createProviderRequestPlan(input: Omit<ProviderRequestPlan, "version" | "fingerprint" | "options"> & {
  options?: Record<string, unknown>
}): ProviderRequestPlan {
  const options = materialOptions(input.options ?? {})
  const plan = {
    version: 1 as const,
    providerId: input.providerId,
    modelId: input.modelId,
    endpoint: clone(input.endpoint),
    context: clone(input.context),
    tools: clone(input.tools),
    options,
  }
  return { ...plan, fingerprint: sha256(stableJson(plan)) }
}

/** Bind a preflight count to its exact provider-visible request. Replay only reuses matching facts. */
export function recordPromptMeasurement(
  plan: Pick<ProviderRequestPlan, "fingerprint">,
  measurement: Omit<RecordedPromptMeasurement, "version" | "requestFingerprint">,
): RecordedPromptMeasurement {
  return {
    version: 1,
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
  return recorded?.version === 1 && recorded.requestFingerprint === plan.fingerprint ? clone(recorded) : undefined
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
  if (!snapshot.version || !snapshot.currency || Number.isNaN(at.valueOf()) || Number.isNaN(from.valueOf())) {
    return { source: "unpriced", reason: "invalid_pricing_snapshot" }
  }
  if (at < from) return { source: "unpriced", reason: "pricing_snapshot_not_effective" }
  if (expires && (Number.isNaN(expires.valueOf()) || at >= expires)) {
    return { source: "unpriced", reason: "pricing_snapshot_expired" }
  }
  const rates = snapshot.ratesPerMillion
  if (Object.values(rates).some(rate => typeof rate !== "number" || !Number.isFinite(rate) || rate < 0)) {
    return { source: "unpriced", reason: "invalid_pricing_snapshot" }
  }
  const amount = (
    usage.uncachedInputTokens * rates.input
    + usage.outputTokens * rates.output
    + (usage.cacheReadInputTokens ?? 0) * (rates.cacheRead ?? rates.input)
    + (usage.cacheCreationInputTokens ?? 0) * (rates.cacheCreation ?? rates.input)
  ) / 1_000_000
  return { source: "snapshot", currency: snapshot.currency, amount, pricingVersion: snapshot.version }
}

function materialOptions(options: Record<string, unknown>): Record<string, unknown> {
  return sanitizeMaterialValue(options) as Record<string, unknown>
}

function sanitizeMaterialValue(value: unknown): unknown {
  if (value === undefined || typeof value === "function") return undefined
  if (Array.isArray(value)) return value.map(sanitizeMaterialValue).filter(item => item !== undefined)
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.keys(value as Record<string, unknown>).sort().flatMap(key => {
      if (TRANSPORT_ONLY_KEYS.has(key)) return []
      const sanitized = sanitizeMaterialValue((value as Record<string, unknown>)[key])
      return sanitized === undefined ? [] : [[key, sanitized]]
    }))
  }
  return value
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`
  const object = value as Record<string, unknown>
  return `{${Object.keys(object).sort().map(key => `${JSON.stringify(key)}:${stableJson(object[key])}`).join(",")}}`
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
