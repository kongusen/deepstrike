"""Workflow primitives — ergonomic helpers over the native kernel state machines.

Re-exports the native :class:`Tournament` and :class:`LoopUntilDone` classes and provides
a small :class:`StopCondition` dataclass for configuring the loop.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

from deepstrike._kernel import (  # noqa: F401
    Tournament,
    TournamentAction,
    TournamentMatch,
    LoopUntilDone,
    LoopAction,
    RoundReport,
)


@dataclass
class StopCondition:
    """A single loop stop predicate. The loop stops as soon as any condition fires.

    ``kind`` is one of ``"no_new_findings"``, ``"no_errors"``, ``"max_rounds"``.
    ``max_rounds`` is required only when ``kind == "max_rounds"``.
    """

    kind: str
    max_rounds: Optional[int] = None

    @staticmethod
    def no_new_findings() -> "StopCondition":
        return StopCondition("no_new_findings")

    @staticmethod
    def no_errors() -> "StopCondition":
        return StopCondition("no_errors")

    @staticmethod
    def max_rounds_at(rounds: int) -> "StopCondition":
        return StopCondition("max_rounds", rounds)


__all__ = [
    "Tournament",
    "TournamentAction",
    "TournamentMatch",
    "LoopUntilDone",
    "LoopAction",
    "RoundReport",
    "StopCondition",
]
