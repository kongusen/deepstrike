from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional

from deepstrike.collaboration.contract import ContractCheckResult, VerificationContract

@dataclass
class HandoffArtifact:
    """
    Structured state passed between sprints / agent instances.

    Every handoff path (context renewal and sub-agent completion)
    produces a HandoffArtifact so the next agent knows not only *what was done*
    but *what has been proven*.
    """
    goal: str
    sprint: int
    progress_summary: str
    open_tasks: list[str] = field(default_factory=list)
    contract_status: list[ContractCheckResult] = field(default_factory=list)
    drift_rate_24h: float = 0.0
    blocked_on: list[str] = field(default_factory=list)


@dataclass
class ContractOutcomeInput:
    contract: VerificationContract
    check_results: list[ContractCheckResult]
    artifact: str
    success: bool
    blocked_on: Optional[list[str]] = None


class HandoffBus:
    """
    Canonical factory for HandoffArtifact.

    Every transition between agent contexts goes through one of these static methods,
    ensuring that the resulting artifact always carries contract_status.
    """

    @staticmethod
    def from_contract_outcome(inp: ContractOutcomeInput) -> HandoffArtifact:
        failed_required = [
            r for r in inp.check_results
            if not r.passed
            and any(c.id == r.criterion_id and c.required for c in inp.contract.acceptance)
        ]
        total = len(inp.check_results)
        drift = len([r for r in inp.check_results if not r.passed]) / total if total > 0 else 0.0

        if inp.success:
            summary = f"Completed: {inp.artifact[:200]}{'…' if len(inp.artifact) > 200 else ''}"
            open_tasks: list[str] = []
        else:
            summary = (
                f"Incomplete after max attempts. "
                f"{len(failed_required)} required criteria failed."
            )
            open_tasks = [f"Fix criterion: {r.criterion_id}" for r in failed_required]

        return HandoffArtifact(
            goal=inp.contract.goal,
            sprint=1,
            progress_summary=summary,
            open_tasks=open_tasks,
            contract_status=inp.check_results,
            drift_rate_24h=drift,
            blocked_on=inp.blocked_on or [],
        )

    @staticmethod
    def from_sub_agent_result(
        *,
        goal: str,
        final_message: str,
        sprint: int = 1,
    ) -> HandoffArtifact:
        return HandoffArtifact(
            goal=goal,
            sprint=sprint,
            progress_summary=final_message[:500],
        )

    @staticmethod
    def to_context_note(artifact: HandoffArtifact) -> str:
        """Render the artifact as a compact string for injection into working context."""
        lines = [
            f"[Handoff from sprint {artifact.sprint}]",
            f"Goal: {artifact.goal}",
            f"Progress: {artifact.progress_summary}",
        ]
        if artifact.open_tasks:
            lines.append(f"Open tasks: {'; '.join(artifact.open_tasks)}")
        if artifact.contract_status:
            passed = sum(1 for r in artifact.contract_status if r.passed)
            lines.append(f"Contract: {passed}/{len(artifact.contract_status)} criteria passed")
        if artifact.blocked_on:
            lines.append(f"BLOCKED ON: {'; '.join(artifact.blocked_on)}")
        if artifact.drift_rate_24h > 0:
            lines.append(f"Drift rate: {artifact.drift_rate_24h * 100:.1f}%")
        return "\n".join(lines)

    @staticmethod
    def requires_escalation(artifact: HandoffArtifact, *, drift_threshold: float = 0.05) -> bool:
        """True when drift rate exceeds threshold or blocked_on is non-empty."""
        return artifact.drift_rate_24h > drift_threshold or bool(artifact.blocked_on)
