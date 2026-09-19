"""
P4 (0.2.64 Execution Evidence Plane): python mirror of the host-side object model connecting
kernel-minted effects to real provider execution — ModelInvocation / ProviderAttempt /
ResolvedProviderRoute / UsageAccountingPolicy. Everything here is L2 host evidence (B7):
it lands in SessionLog for cross-verification against the journal (C6/C8) and is NEVER
fed back as kernel input (B4/DEC-2 discipline: wall-clock and wire facts stay host-side).
"""

from typing import TypedDict, Literal, Protocol, Any

from deepstrike.providers.request_plan import (
    NormalizedProviderUsage,
    ResolvedProviderRoute,
    normalize_provider_usage,
)
from deepstrike.providers.usage import ProviderUsage

# Canonical stop-reason vocabulary already carried on the wire usage frame (types.py).
CanonicalStopReason = Literal["end_turn", "max_tokens", "stop_sequence", "tool_use", "content_filtered"]


class ProviderWireEvidence(TypedDict, total=False):
    """P4 §1.2: host-observed wire facts for one provider execution."""
    protocol: str  # required
    request_fingerprint: str  # required, → ProviderRequestPlan.fingerprint (G2)
    response_id: str  # optional, OpenAI response id / Anthropic message id
    raw_usage: Any  # optional, BoundedJson semantics: truncated to ≤4KB
    replay_state: dict[str, Any]  # optional, provider-native replay state

class ModelUsageSettlement(TypedDict):
    """
    P4 §2: the only two numbers that cross the kernel boundary (B4). Field names match the
    existing ResolveEffect wire shape — this is what the runner already feeds the kernel as
    `observed_input_tokens` / `observed_output_tokens`.
    """
    observed_input_tokens: int
    observed_output_tokens: int


class UsageAccountingPolicy(Protocol):
    """
    P4 §2.1: the named, pinnable, replayable policy turning a measurement into a settlement.
    `settle` must be a pure function of the measurement — given (usage, policy_id) any auditor
    recomputes the identical settlement.
    """
    policy_id: str

    def settle(self, usage: NormalizedProviderUsage) -> ModelUsageSettlement:
        ...


class _FullFootprintPolicy:
    """
    The default policy = today's implicit runner behavior exactly (full input footprint + full
    output footprint — the two numbers the runner has always fed `observed_*`). P4 changes no
    default numbers; it only makes the conversion a named object. The date stamp is the
    policy's identity: any future semantics change MUST ship under a new policy_id.
    """
    policy_id = "deepstrike.full-footprint@2026-09-15"

    def settle(self, usage: NormalizedProviderUsage) -> ModelUsageSettlement:
        return {
            "observed_input_tokens": usage.input_tokens,
            "observed_output_tokens": usage.output_tokens,
        }


FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY: UsageAccountingPolicy = _FullFootprintPolicy()


def route_to_record(route: ResolvedProviderRoute) -> dict[str, Any]:
    """ResolvedProviderRoute → its SessionLog JSON shape (python-native snake_case fields —
    same nested-shape convention as `prompt_measured.measurement`; the chain validator reads
    both spellings)."""
    return {
        "route_id": route.route_id,
        "provider": route.provider,
        "protocol": route.protocol,
        "model": route.model,
        "endpoint": {
            "id": route.endpoint.id,
            "protocol": route.endpoint.protocol,
            "base_url": route.endpoint.base_url,
        },
        "adapter_version": route.adapter_version,
        "capabilities_ref": route.capabilities_ref,
    }


def try_normalize_provider_usage(usage: ProviderUsage) -> NormalizedProviderUsage | None:
    """Defensive measurement assembly for the attempt evidence path: an invalid frame (cache
    subsets exceeding input, etc.) degrades to NO measurement instead of breaking the run —
    evidence is never worth a run failure, and the settlement falls back to the raw counts."""
    try:
        return normalize_provider_usage(usage)
    except (TypeError, ValueError):
        return None


def usage_to_record(
    usage: NormalizedProviderUsage,
    *,
    cache_telemetry_status: str = "unavailable",
    cache_telemetry_source: str | None = None,
) -> dict[str, Any]:
    """NormalizedProviderUsage → its SessionLog JSON shape, with the telemetry fields
    normalization drops re-attached so `usage` stays full-field."""
    record: dict[str, Any] = {
        "input_tokens": usage.input_tokens,
        "uncached_input_tokens": usage.uncached_input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_input_tokens,
        "cache_creation_input_tokens": usage.cache_creation_input_tokens,
        "cache_telemetry_status": cache_telemetry_status,
    }
    if usage.reasoning_tokens is not None:
        record["reasoning_tokens"] = usage.reasoning_tokens
    if cache_telemetry_source is not None:
        record["cache_telemetry_source"] = cache_telemetry_source
    return record


ProviderAttemptStatus = Literal["success", "transport_exhausted", "aborted", "rejected"]


class ProviderAttempt(TypedDict, total=False):
    """
    P4 §1.2: one effect's execution against one route, one physical attempt. The kernel-minted
    `effect_id` is the primary key (H7 — no parallel id minting). Transport-ladder rungs are
    summarized as a count plus the final error class; rung-level evidence belongs to adapter
    debug logs, not SessionLog.
    """
    effect_id: str  # required
    attempt_seq: int  # required, 1-based, always 1 today (P4 §0.2)
    route: ResolvedProviderRoute  # required
    request_fingerprint: str  # required, → ProviderRequestPlan.fingerprint (G2)
    status: ProviderAttemptStatus  # required
    transport_rungs: int  # required
    last_error_class: str  # optional, classifyProviderError's class
    started_at_ms: int  # required, host wall-clock, pure evidence
    finished_at_ms: int  # required, host wall-clock, pure evidence
    usage: NormalizedProviderUsage  # optional, full measurement fields
    wire_evidence: ProviderWireEvidence  # optional


class ProviderAttemptRecord(TypedDict, total=False):
    """
    The SessionLog wire payload of a ProviderAttempt (P4 §3): the §1.2 fields flattened into
    the event, snake_case per SessionLog convention. Nested objects keep their native shape
    (same convention as `prompt_measured.measurement`).
    """
    effect_id: str  # required
    attempt_seq: int  # required
    route: ResolvedProviderRoute  # required
    request_fingerprint: str  # required
    status: ProviderAttemptStatus  # required
    transport_rungs: int  # required
    last_error_class: str  # optional
    started_at_ms: int  # required
    finished_at_ms: int  # required
    usage: NormalizedProviderUsage  # optional
    wire_evidence: ProviderWireEvidence  # optional
    accounting_policy_id: str  # optional, P4 §2.1


def provider_attempt_to_record(
    attempt: "ProviderAttempt",
    accounting_policy_id: str | None = None,
) -> "ProviderAttemptRecord":
    """ProviderAttempt → its SessionLog wire payload. The accounting policy id pins only when
    the caller passes it (the runner does so iff a measurement exists), so (usage, policy_id)
    deterministically recomputes the settlement that crossed the wire."""
    record: ProviderAttemptRecord = {
        "effect_id": attempt["effect_id"],
        "attempt_seq": attempt["attempt_seq"],
        "route": attempt["route"],
        "request_fingerprint": attempt["request_fingerprint"],
        "status": attempt["status"],
        "transport_rungs": attempt["transport_rungs"],
        "started_at_ms": attempt["started_at_ms"],
        "finished_at_ms": attempt["finished_at_ms"],
    }
    if attempt.get("last_error_class") is not None:
        record["last_error_class"] = attempt["last_error_class"]
    if attempt.get("usage") is not None:
        record["usage"] = attempt["usage"]
    if attempt.get("wire_evidence") is not None:
        record["wire_evidence"] = attempt["wire_evidence"]
    if accounting_policy_id is not None:
        record["accounting_policy_id"] = accounting_policy_id
    return record


class InvocationOutcome(TypedDict, total=False):
    """P4 §1.4: the invocation's terminal projection."""
    invocation_id: str  # required
    selected_effect_id: str  # required, the effect the kernel adopted as the outcome
    stop_reason: CanonicalStopReason  # optional
    settlement: ModelUsageSettlement  # optional, absent on a failed chain


class ModelInvocation(TypedDict, total=False):
    """
    P4 §1.1: one logical model call = the fact-connected chain of CallProvider effects the
    kernel walked to obtain one turn of model output. Derived identity, zero minting:
    `invocation_id` IS the first effect's effect_id. Authority = journal; the SessionLog
    projection (`llm_completed.invocation_id`) is evidence only.
    """
    invocation_id: str  # required
    turn: int  # required
    effect_chain: list[str]  # required, every effect_id on the chain in order
    outcome: InvocationOutcome  # optional
