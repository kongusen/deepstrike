import type { RenderedContext, ToolSchema } from "../types.js"
import { sha256Hex } from "../runtime/canonical-kernel-step.js"

export interface ProviderRequestEndpoint { id: string; protocol: string; baseURL: string }
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
export interface RecordedPromptMeasurement {
  version: 1
  requestFingerprint: string
  inputTokens: number
  source: { kind: "native"; provider: string } | { kind: "local_exact"; tokenizer: string } | { kind: "heuristic" }
  confidence: "exact" | "high_confidence" | "low_confidence"
}

const EXCLUDED = new Set(["apiKey", "api_key", "bearerToken", "bearer_token", "authorization", "credential", "credentials", "retry", "maxRetries", "baseDelay", "timeout", "signal"])

export function createProviderRequestPlan(input: Omit<ProviderRequestPlan, "version" | "fingerprint" | "options"> & { options?: Record<string, unknown> }): ProviderRequestPlan {
  const value = {
    version: 1 as const, providerId: input.providerId, modelId: input.modelId, endpoint: clone(input.endpoint),
    context: clone(input.context), tools: clone(input.tools), options: materialOptions(input.options ?? {}),
  }
  return { ...value, fingerprint: sha256Hex(stableJson(value)) }
}

export function recordPromptMeasurement(plan: Pick<ProviderRequestPlan, "fingerprint">, measurement: Omit<RecordedPromptMeasurement, "version" | "requestFingerprint">): RecordedPromptMeasurement {
  if (!Number.isSafeInteger(measurement.inputTokens) || measurement.inputTokens < 0) throw new RangeError("inputTokens must be a non-negative safe integer")
  return { version: 1, requestFingerprint: plan.fingerprint, inputTokens: measurement.inputTokens, source: clone(measurement.source), confidence: measurement.confidence }
}

export function measurementForPlan(plan: Pick<ProviderRequestPlan, "fingerprint">, record: RecordedPromptMeasurement | undefined): RecordedPromptMeasurement | undefined {
  return record?.version === 1 && record.requestFingerprint === plan.fingerprint ? clone(record) : undefined
}

function materialOptions(options: Record<string, unknown>): Record<string, unknown> {
  return sanitizeMaterialValue(options) as Record<string, unknown>
}

function sanitizeMaterialValue(value: unknown): unknown {
  if (value === undefined || typeof value === "function") return undefined
  if (Array.isArray(value)) return value.map(sanitizeMaterialValue).filter(item => item !== undefined)
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.keys(value as Record<string, unknown>).sort().flatMap(key => {
      if (EXCLUDED.has(key)) return []
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

function clone<T>(value: T): T {
  if (value === undefined || value === null || typeof value !== "object") return value
  if (Array.isArray(value)) return value.map(clone) as T
  return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, item]) => [key, clone(item)])) as T
}
