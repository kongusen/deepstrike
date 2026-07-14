from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable


@dataclass(frozen=True)
class ObserverFailure:
    component: str
    operation: str
    cause: BaseException
    committed: bool = True


ObserverErrorHandler = Callable[[ObserverFailure], Any]


def report_observer_failure(
    handler: ObserverErrorHandler | None,
    *,
    component: str,
    operation: str,
    cause: BaseException,
) -> None:
    """Report an observer failure without changing the already committed result."""
    if handler is None:
        return
    try:
        handler(ObserverFailure(component=component, operation=operation, cause=cause))
    except Exception:
        # The reporter is itself an observer; it cannot become a second semantic owner.
        pass
