from __future__ import annotations

import math
import re
from dataclasses import dataclass
from typing import Callable, Generic, TypeVar

T = TypeVar("T")
_SEGMENT = re.compile(r"[^\W_]+", re.UNICODE)
_DAY_MS = 86_400_000


def _is_han(character: str) -> bool:
    codepoint = ord(character)
    return (
        0x3400 <= codepoint <= 0x4DBF
        or 0x4E00 <= codepoint <= 0x9FFF
        or 0xF900 <= codepoint <= 0xFAFF
        or 0x20000 <= codepoint <= 0x3134F
    )


def terms(text: str) -> set[str]:
    result: set[str] = set()
    for segment in _SEGMENT.findall(text.casefold()):
        result.add(segment)
        if any(_is_han(character) for character in segment):
            result.update(segment[index:index + 2] for index in range(len(segment) - 1))
    return result


@dataclass
class RankedMemory(Generic[T]):
    """One ranked hit with a genuine relevance score in [0,1] and a rationale."""
    value: T
    score: float
    why: str


def _staleness_penalty(
    updated_at: int, ttl_days: int | None, now_ms: int | None, stale_warning_days: int | None
) -> float:
    """Day-based staleness discount in [0, 0.9). Clock-based, so it lives host-side."""
    if now_ms is None:
        return 0.0
    age_days = max(0.0, (now_ms - updated_at) / _DAY_MS)
    penalty = 0.0
    if stale_warning_days is not None and age_days > stale_warning_days:
        penalty += min(0.3, 0.05 * (age_days - stale_warning_days))
    if ttl_days is not None and age_days > ttl_days:
        penalty += 0.4
    return min(0.9, penalty)


def _recall_boost(recall_count: int) -> float:
    if recall_count <= 0:
        return 0.0
    return min(0.15, 0.05 * math.log2(1 + recall_count))


def rank_memories(
    query: str,
    candidates: list[T],
    top_k: int,
    *,
    searchable_text: Callable[[T], str],
    updated_at: Callable[[T], int],
    recall_count: Callable[[T], int] = lambda _c: 0,
    ttl_days: Callable[[T], int | None] = lambda _c: None,
    now_ms: int | None = None,
    stale_warning_days: int | None = None,
) -> list[RankedMemory[T]]:
    """Rank memories without embeddings, returning a genuine relevance score in [0,1].

    Score is lexical overlap (fraction of distinct query terms present) as the dominant term,
    lifted slightly by recall history and lowered by TTL/staleness. Recency and insertion order
    break ties. A non-empty query never returns unrelated entries. Score is relevance, deliberately
    distinct from a record's stored confidence.
    """
    query_terms = terms(query)
    limit = max(0, int(top_k))
    if limit == 0:
        return []
    ranked: list[tuple[float, int, int, int, RankedMemory[T]]] = []
    for insertion_index, candidate in enumerate(candidates):
        candidate_terms = terms(searchable_text(candidate))
        matches = sum(1 for term in query_terms if term in candidate_terms)
        if query_terms and matches == 0:
            continue
        fraction = 0.0 if not query_terms else matches / len(query_terms)
        penalty = _staleness_penalty(updated_at(candidate), ttl_days(candidate), now_ms, stale_warning_days)
        boost = _recall_boost(recall_count(candidate))
        score = max(0.0, min(1.0, fraction * 0.85 + boost - penalty))
        if not query_terms:
            why = "no query terms; insertion order"
        else:
            why = f"lexical {matches}/{len(query_terms)}"
            if boost > 0:
                why += f", recall×{recall_count(candidate)}"
            if penalty > 0:
                why += f", stale -{penalty:.2f}"
        # Sort key: score desc, matches desc, recency desc, insertion asc.
        ranked.append((-score, -matches, -updated_at(candidate), insertion_index, RankedMemory(candidate, score, why)))
    ranked.sort(key=lambda row: row[:4])
    return [row[4] for row in ranked[:limit]]
