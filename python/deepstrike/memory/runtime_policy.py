"""Host-owned memory path switches, independent of the kernel's admission policy."""
from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Literal

MemoryRetrievalPolicy = Literal["disabled", "host"]
MemoryWritePolicy = Literal["disabled", "candidate_sink"]


@dataclass(frozen=True)
class MemoryRuntimePolicy:
    retrieval: MemoryRetrievalPolicy
    session_extraction: MemoryWritePolicy
    model_writes: MemoryWritePolicy
    semantic_archive: MemoryWritePolicy


@dataclass(frozen=True)
class ResolvedMemoryRuntimePolicy:
    retrieval_enabled: bool
    session_extraction_enabled: bool
    model_writes_enabled: bool
    semantic_archive_enabled: bool
    session_extraction_to_candidate_sink: bool
    model_writes_to_candidate_sink: bool
    semantic_archive_to_candidate_sink: bool


def resolve_memory_runtime_policy(
    policy: MemoryRuntimePolicy | dict[str, str] | None,
) -> ResolvedMemoryRuntimePolicy | None:
    """Validate all four switches. No policy preserves the existing store-backed paths."""
    if policy is None:
        return None
    if isinstance(policy, MemoryRuntimePolicy):
        values = asdict(policy)
    elif isinstance(policy, dict):
        values = policy
    else:
        raise TypeError("memory runtime policy must be MemoryRuntimePolicy or a dict")
    fields = ("retrieval", "session_extraction", "model_writes", "semantic_archive")
    unknown = set(values) - set(fields)
    if unknown:
        raise ValueError(f"unknown memory runtime policy field(s): {', '.join(sorted(unknown))}")
    for field in fields:
        allowed = ("disabled", "host") if field == "retrieval" else ("disabled", "candidate_sink")
        if values.get(field) not in allowed:
            raise ValueError(f"memory runtime policy {field} must be one of {' | '.join(allowed)}, got {values.get(field)!r}")
    return ResolvedMemoryRuntimePolicy(
        retrieval_enabled=values["retrieval"] != "disabled",
        session_extraction_enabled=values["session_extraction"] != "disabled",
        model_writes_enabled=values["model_writes"] != "disabled",
        semantic_archive_enabled=values["semantic_archive"] != "disabled",
        session_extraction_to_candidate_sink=values["session_extraction"] == "candidate_sink",
        model_writes_to_candidate_sink=values["model_writes"] == "candidate_sink",
        semantic_archive_to_candidate_sink=values["semantic_archive"] == "candidate_sink",
    )
