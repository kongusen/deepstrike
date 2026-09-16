/**
 * P4 (0.2.64 Execution Evidence Plane): the host-side object model connecting a kernel-minted
 * effect to the real provider execution it drove — ModelInvocation / ProviderAttempt /
 * ResolvedProviderRoute / UsageAccountingPolicy. Everything here is L2 host evidence
 * (B7): it lands in SessionLog for cross-verification against the journal (C6/C8) and is
 * NEVER fed back as kernel input (B4/DEC-2 discipline: wall-clock and wire facts stay host-side).
 *
 * Kernel ABI is untouched — the kernel projection of an invocation outcome remains the
 * existing ProviderCompleted/HostEffectFailure shapes (D2).
 */
import type { ProviderUsage, ProviderWireEvidence, UsageEvent } from "../types.js"
import {
  normalizeProviderUsage,
  type NormalizedProviderUsage,
  type ResolvedProviderRoute,
} from "../providers/request-plan.js"

/** Canonical stop-reason vocabulary already carried on the wire usage frame (types.ts). */
export type CanonicalStopReason = NonNullable<UsageEvent["stopReason"]>

/**
 * P4 §1.1: one logical model call = the fact-connected chain of CallProvider effects the
 * kernel walked to obtain one turn of model output. Derived identity, zero minting:
 * `invocationId` IS the first effect's effect_id. Authority = journal; the SessionLog
 * projection (`llm_completed.invocation_id`) is evidence only.
 */
export interface ModelInvocation {
  invocationId: string
  turn: number
  /** Every effect_id on the chain, in order. Length 1 = first attempt succeeded. */
  effectChain: string[]
  outcome?: InvocationOutcome
}

/** P4 §1.4: the invocation's terminal projection. */
export interface InvocationOutcome {
  invocationId: string
  /** The effect the kernel adopted as the outcome (the successful one). */
  selectedEffectId: string
  stopReason?: CanonicalStopReason
  /** Absent on a failed chain (nothing settled). */
  settlement?: ModelUsageSettlement
}

export type ProviderAttemptStatus = "success" | "transport_exhausted" | "aborted" | "rejected"

/**
 * P4 §1.2: one effect's execution against one route, one physical attempt. The kernel-minted
 * `effectId` is the primary key (H7 — no parallel id minting). Transport-ladder rungs are
 * summarized as a count plus the final error class; rung-level evidence belongs to adapter
 * debug logs, not SessionLog.
 */
export interface ProviderAttempt {
  effectId: string
  /** Failover sequence within one effect execution (1-based). Always 1 today (P4 §0.2). */
  attemptSeq: number
  route: ResolvedProviderRoute
  /** → ProviderRequestPlan.fingerprint (G2). */
  requestFingerprint: string
  status: ProviderAttemptStatus
  transportRungs: number
  /** classifyProviderError's class, never the raw vendor text (B1 spirit). */
  lastErrorClass?: string
  /** Host wall-clock, pure evidence, never kernel input (DEC-2 discipline). */
  startedAtMs: number
  finishedAtMs: number
  /** Full measurement fields (P4 §2); only the settlement crosses the kernel boundary (B4). */
  usage?: NormalizedProviderUsage
  wireEvidence?: ProviderWireEvidence
}

/**
 * The SessionLog wire payload of a ProviderAttempt (P4 §3): the §1.2 fields flattened into
 * the event, snake_case per SessionLog convention. Nested objects keep their native shape
 * (same convention as `prompt_measured.measurement`).
 */
export interface ProviderAttemptRecord {
  effect_id: string
  attempt_seq: number
  route: ResolvedProviderRoute
  request_fingerprint: string
  status: ProviderAttemptStatus
  transport_rungs: number
  last_error_class?: string
  started_at_ms: number
  finished_at_ms: number
  usage?: NormalizedProviderUsage
  wire_evidence?: ProviderWireEvidence
  /** P4 §2.1: the accounting policy this attempt's settlement was/will be derived with —
   *  pinned on the attempt so settlement is deterministically recomputable from
   *  (measurement, policy_id). */
  accounting_policy_id?: string
}

export function providerAttemptToRecord(attempt: ProviderAttempt, accountingPolicyId?: string): ProviderAttemptRecord {
  return {
    effect_id: attempt.effectId,
    attempt_seq: attempt.attemptSeq,
    route: attempt.route,
    request_fingerprint: attempt.requestFingerprint,
    status: attempt.status,
    transport_rungs: attempt.transportRungs,
    ...(attempt.lastErrorClass !== undefined ? { last_error_class: attempt.lastErrorClass } : {}),
    started_at_ms: attempt.startedAtMs,
    finished_at_ms: attempt.finishedAtMs,
    ...(attempt.usage !== undefined ? { usage: attempt.usage } : {}),
    ...(attempt.wireEvidence !== undefined ? { wire_evidence: attempt.wireEvidence } : {}),
    ...(accountingPolicyId !== undefined ? { accounting_policy_id: accountingPolicyId } : {}),
  }
}

/**
 * P4 §2: the only two numbers that cross the kernel boundary (B4). Field names match the
 * existing ResolveEffect wire shape — this is what the runner already feeds the kernel as
 * `observed_input_tokens` / `observed_output_tokens`.
 */
export interface ModelUsageSettlement {
  observed_input_tokens: number
  observed_output_tokens: number
}

/**
 * P4 §2.1: the named, pinnable, replayable policy turning a measurement into a settlement.
 * `settle` must be a pure function of the measurement — given (usage, policyId) any auditor
 * recomputes the identical settlement.
 */
export interface UsageAccountingPolicy {
  policyId: string
  settle(usage: NormalizedProviderUsage): ModelUsageSettlement
}

/**
 * The default policy = today's implicit runner behavior exactly (full input footprint + full
 * output footprint — the two numbers the runner has always fed `observed_*`). P4 changes no
 * default numbers; it only makes the conversion a named object. The date stamp is the
 * policy's identity: any future semantics change MUST ship under a new policyId.
 */
export const FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY: UsageAccountingPolicy = {
  policyId: "deepstrike.full-footprint@2026-09-15",
  settle(usage: NormalizedProviderUsage): ModelUsageSettlement {
    return {
      observed_input_tokens: usage.inputTokens,
      observed_output_tokens: usage.outputTokens,
    }
  },
}

/**
 * Defensive measurement assembly for the attempt evidence path: an invalid frame (cache
 * subsets exceeding input, etc.) degrades to NO measurement instead of breaking the run —
 * evidence is never worth a run failure, and the settlement falls back to the raw counts.
 * Telemetry fields normalizeProviderUsage drops are re-attached so `usage` stays full-field.
 */
export function tryNormalizeProviderUsage(usage: ProviderUsage): NormalizedProviderUsage | undefined {
  try {
    const normalized = normalizeProviderUsage(usage)
    return {
      ...normalized,
      ...(usage.cacheTelemetryStatus !== undefined ? { cacheTelemetryStatus: usage.cacheTelemetryStatus } : {}),
      ...(usage.cacheTelemetrySource !== undefined ? { cacheTelemetrySource: usage.cacheTelemetrySource } : {}),
    }
  } catch {
    return undefined
  }
}
