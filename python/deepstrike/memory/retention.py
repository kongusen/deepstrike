"""Host-side mirror of the kernel's shared retention vocabulary (`crates/.../mm/value.rs` +
`memory_retention_score`). The durable store owns the full cross-session record set, so it — not the
kernel — enforces the capacity bound; it ranks by the same integer "value" definition so memory and
context knowledge agree on what is worth keeping.

The turn-based recency term is omitted here (the host store has no turn counter), so recall_count,
kind, confidence, size, and clock-based staleness drive the ordering. Parity-tested against the
Rust reference for the terms both compute.
"""
from __future__ import annotations

import math

from deepstrike.memory.protocols import MemoryKind, MemoryRecord

_DAY_MS = 86_400_000
_KIND_WEIGHT: dict[MemoryKind, int] = {
    "user": 1_600,
    "feedback": 1_800,
    "project": 1_400,
    "reference": 1_200,
}


def _usage_bucket(use_count: int) -> int:
    """floor(log2(1 + n)) — the kernel's stable ln(1+n) proxy for usage."""
    if use_count <= 0:
        return 0
    return int(math.floor(math.log2(use_count + 1)))


def _stale_discount_ppm(updated_at: int, ttl_days: int | None, now_ms: int, stale_warning_days: int) -> int:
    age_days = max(0.0, (now_ms - updated_at) / _DAY_MS)
    ppm = 0.0
    if age_days > stale_warning_days:
        ppm += min(300_000, 50_000 * (age_days - stale_warning_days))
    if ttl_days is not None and age_days > ttl_days:
        ppm += 400_000
    return min(1_000_000, int(math.floor(ppm)))


def memory_retention_score(record: MemoryRecord, now_ms: int, stale_warning_days: int) -> float:
    """Deterministic retention score for a durable record. Higher is retained first;
    ``math.inf`` is reserved for pins (the kernel uses ``i64::MAX``)."""
    if record.pinned:
        return math.inf

    usage = _usage_bucket(record.recall_count) * 8_192
    # Recency (turn-based in the kernel) is unavailable host-side; omitted deterministically.
    kind = _KIND_WEIGHT.get(record.kind, 0)
    confidence_ppm = min(1_000_000, int(math.floor(max(0.0, min(1.0, record.confidence)) * 1_000_000)))
    confidence = confidence_ppm // 250
    stale_ppm = _stale_discount_ppm(record.updated_at, record.ttl_days, now_ms, stale_warning_days)
    staleness = stale_ppm // 125
    # 4-bytes-per-token proxy, matching the kernel's content.len()/4 (UTF-8 bytes).
    tokens = min(0xFFFFFFFF, len(record.content.encode("utf-8")) // 4)
    size = tokens * 4

    return usage + kind + confidence - staleness - size
