from __future__ import annotations

import re
from typing import Any, Callable, TypeVar

T = TypeVar("T")
_SEGMENT = re.compile(r"[^\W_]+", re.UNICODE)


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


def rank_memories(
    query: str,
    candidates: list[T],
    top_k: int,
    *,
    searchable_text: Callable[[T], str],
    updated_at: Callable[[T], int],
) -> list[T]:
    query_terms = terms(query)
    limit = max(0, int(top_k))
    if limit == 0:
        return []
    ranked: list[tuple[int, int, int, T]] = []
    for insertion_index, candidate in enumerate(candidates):
        candidate_terms = terms(searchable_text(candidate))
        matches = sum(1 for term in query_terms if term in candidate_terms)
        if query_terms and matches == 0:
            continue
        ranked.append((-matches, -updated_at(candidate), insertion_index, candidate))
    ranked.sort(key=lambda row: row[:3])
    return [row[3] for row in ranked[:limit]]
