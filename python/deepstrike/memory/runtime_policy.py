"""Host-owned memory path switches, independent of the kernel's admission policy."""
from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Literal

MemoryRetrievalPolicy = Literal["disabled", "host"]
MemoryWritePolicy = Literal["disabled", "candidate_sink"]


@dataclass(frozen=True)
class MemoryRuntimePolicy:
    retrieval: MemoryRetrievalPolicy
    model_writes: MemoryWritePolicy


@dataclass(frozen=True)
class ResolvedMemoryRuntimePolicy:
    retrieval_enabled: bool
    model_writes_enabled: bool
    model_writes_to_candidate_sink: bool


def resolve_memory_runtime_policy(
    policy: MemoryRuntimePolicy | dict[str, str] | None,
) -> ResolvedMemoryRuntimePolicy | None:
    """Validate both switches. No policy preserves the existing store-backed paths."""
    if policy is None:
        return None
    if isinstance(policy, MemoryRuntimePolicy):
        values = asdict(policy)
    elif isinstance(policy, dict):
        values = policy
    else:
        raise TypeError("memory runtime policy must be MemoryRuntimePolicy or a dict")
    fields = ("retrieval", "model_writes")
    unknown = set(values) - set(fields)
    if unknown:
        raise ValueError(f"unknown memory runtime policy field(s): {', '.join(sorted(unknown))}")
    for field in fields:
        allowed = ("disabled", "host") if field == "retrieval" else ("disabled", "candidate_sink")
        if values.get(field) not in allowed:
            raise ValueError(f"memory runtime policy {field} must be one of {' | '.join(allowed)}, got {values.get(field)!r}")
    return ResolvedMemoryRuntimePolicy(
        retrieval_enabled=values["retrieval"] != "disabled",
        model_writes_enabled=values["model_writes"] != "disabled",
        model_writes_to_candidate_sink=values["model_writes"] == "candidate_sink",
    )
