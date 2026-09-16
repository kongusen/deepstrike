"""
P4 (0.2.64 Execution Evidence Plane): python mirror of the host-side object model connecting
kernel-minted effects to real provider execution — ModelInvocation / ProviderAttempt /
ResolvedProviderRoute / UsageAccountingPolicy. Everything here is L2 host evidence (B7):
it lands in SessionLog for cross-verification against the journal (C6/C8) and is NEVER
fed back as kernel input (B4/DEC-2 discipline: wall-clock and wire facts stay host-side).
"""

from typing import TypedDict, Literal, Protocol, Any
from deepstrike.providers.request_plan import NormalizedProviderUsage, ResolvedProviderRoute

# Canonical stop-reason vocabulary already carried on the wire usage frame (types.py).
CanonicalStopReason = Literal["end_turn", "max_tokens", "stop_sequence", "tool_use", "content_filtered"]


class ProviderWireEvidence(TypedDict, total=False):
    """P4 §1.2: host-observed wire facts for one provider execution."""
    protocol: str  # required
    request_fingerprint: str  # required, → ProviderRequestPlan.fingerprint (G2)
    response_id: str  # optional, OpenAI response id / Anthropic message id
    raw_usage: Any  # optional, BoundedJson semantics: truncated to ≤4KB
    replay_state: dict[str, Any]  # optional, former llm_completed.provider_replay

class UsageSettlement(TypedDict):
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

    def settle(self, usage: NormalizedProviderUsage) -> UsageSettlement:
        ...


class _FullFootprintPolicy:
    """
    The default policy = today's implicit runner behavior exactly (full input footprint + full
    output footprint — the two numbers the runner has always fed `observed_*`). P4 changes no
    default numbers; it only makes the conversion a named object. The date stamp is the
    policy's identity: any future semantics change MUST ship under a new policy_id.
    """
    policy_id = "deepstrike.full-footprint@2026-09-15"

    def settle(self, usage: NormalizedProviderUsage) -> UsageSettlement:
        return {
            "observed_input_tokens": usage["input_tokens"],
            "observed_output_tokens": usage["output_tokens"],
        }


FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY: UsageAccountingPolicy = _FullFootprintPolicy()


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


class InvocationOutcome(TypedDict, total=False):
    """P4 §1.4: the invocation's terminal projection."""
    invocation_id: str  # required
    selected_effect_id: str  # required, the effect the kernel adopted as the outcome
    stop_reason: CanonicalStopReason  # optional
    settlement: UsageSettlement  # optional, absent on a failed chain


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
