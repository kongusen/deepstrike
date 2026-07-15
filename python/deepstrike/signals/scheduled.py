from __future__ import annotations
from dataclasses import dataclass, field
from typing import Any


@dataclass
class ScheduledPrompt:
    goal: str
    run_at_ms: int                        # epoch ms
    criteria: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_signal(self) -> "RuntimeSignal":
        from deepstrike.signals.types import RuntimeSignal
        return RuntimeSignal(
            source="cron",
            signal_type="job",
            urgency=self.metadata.get("urgency", "normal"),
            payload={"goal": self.goal, "criteria": self.criteria, **self.metadata},
            dedupe_key=f"cron:{self.goal}:{self.run_at_ms}",
        )
