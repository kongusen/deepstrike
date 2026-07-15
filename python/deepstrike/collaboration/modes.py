from __future__ import annotations

import json
import re
import uuid
from dataclasses import dataclass

from deepstrike.collaboration.contract import (
    AcceptanceCriterion,
    ContractCheckResult,
    VerificationContract,
)
from deepstrike.collaboration.harness import (
    ContractOutcome,
    CreatorVerifierBody,
    StructuredContractJudge,
)
from deepstrike.collaboration.handoff import ContractOutcomeInput, HandoffBus
from deepstrike.harness.harness import AttemptLoop, AttemptRequest, Criterion, StopPolicy

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from deepstrike.collaboration.pool import AgentPool


@dataclass
class CreatorVerifierMetrics:
    total: int
    failed: int
    drift_rate: float


class CreatorVerifierMode:
    """
    The simplest multi-agent collaboration pattern.

    By default uses the kernel spawn path (``AgentPool.ensure_coordinator``).
    Pass ``use_legacy_runners=True`` to fall back to independent ``runner.run()`` sessions.
    """

    def __init__(
        self,
        pool: "AgentPool",
        *,
        max_attempts: int = 3,
        coordinator_session_id: str | None = None,
    ) -> None:
        self._pool = pool
        self._max_attempts = max_attempts
        self._coordinator_session_id = coordinator_session_id
        self._total = 0
        self._failed = 0

    async def run(self, contract: VerificationContract) -> ContractOutcome:
        self._total += 1
        self._pool.ensure_coordinator(self._coordinator_session_id)
        loop = AttemptLoop(
            body=CreatorVerifierBody(self._pool, contract),
            judge=StructuredContractJudge(self._pool, contract),
            stop=StopPolicy(max_attempts=self._max_attempts),
        )
        attempt = await loop.run(AttemptRequest(
            session_id=str(uuid.uuid4()),
            goal=contract.goal,
            criteria=[
                Criterion(
                    id=criterion.id,
                    text=criterion.text,
                    required=criterion.required,
                    weight=criterion.weight,
                    machine_checkable=criterion.machine_checkable,
                )
                for criterion in contract.acceptance
            ],
        ))
        check_results = [
            ContractCheckResult(
                criterion_id=detail.criterion,
                passed=detail.passed,
                evidence=detail.feedback,
            )
            for detail in (attempt.verdict.details if attempt.verdict else [])
        ]
        success = attempt.outcome == "passed"
        if not success:
            self._failed += 1
        blocked_on = [
            f"[{result.criterion_id}] {result.evidence or 'verification failed'}"
            for result in check_results
            if not result.passed
        ]
        return ContractOutcome(
            success=success,
            artifact=attempt.result,
            check_results=check_results,
            attempts_used=attempt.attempts,
            total_tokens_consumed=attempt.total_tokens,
            handoff=HandoffBus.from_contract_outcome(ContractOutcomeInput(
                contract=contract,
                check_results=check_results,
                artifact=attempt.result,
                success=success,
                blocked_on=blocked_on or None,
            )),
        )

    def get_metrics(self) -> CreatorVerifierMetrics:
        drift = self._failed / self._total if self._total > 0 else 0.0
        return CreatorVerifierMetrics(
            total=self._total,
            failed=self._failed,
            drift_rate=drift,
        )

    def is_drifting(self, threshold: float = 0.05) -> bool:
        return self.get_metrics().drift_rate > threshold

    def reset_metrics(self) -> None:
        self._total = 0
        self._failed = 0


class OrchestrationMode:
    """
    Three-role collaboration: orchestrator → executor → verifier.

    The orchestrator produces a VerificationContract from a raw goal, then
    CreatorVerifierMode executes it. Requires all three roles in the pool.
    """

    def __init__(
        self,
        pool: "AgentPool",
        *,
        max_attempts: int = 3,
        coordinator_session_id: str | None = None,
    ) -> None:
        self._pool = pool
        self._inner = CreatorVerifierMode(
            pool,
            max_attempts=max_attempts,
            coordinator_session_id=coordinator_session_id,
        )

    async def run(self, goal: str) -> tuple[ContractOutcome, VerificationContract]:
        self._pool.ensure_coordinator(self._inner._coordinator_session_id)
        contract_json = await self._pool.run_orchestrator(goal)
        contract = self._parse_contract(contract_json, goal)
        outcome = await self._inner.run(contract)
        return outcome, contract

    def get_metrics(self) -> CreatorVerifierMetrics:
        return self._inner.get_metrics()

    def is_drifting(self, threshold: float = 0.05) -> bool:
        return self._inner.is_drifting(threshold)

    def _parse_contract(self, json_text: str, fallback_goal: str) -> VerificationContract:
        try:
            match = re.search(r"```(?:json)?\s*([\s\S]*?)```", json_text)
            raw_text = match.group(1) if match else json_text
            raw = json.loads(raw_text)
            return VerificationContract(
                id=str(raw.get("id", "orchestrated")),
                goal=str(raw.get("goal", fallback_goal)),
                acceptance=[
                    AcceptanceCriterion(
                        id=str(c.get("id", "criterion")),
                        text=str(c.get("text", "")),
                        required=c.get("required", True) is not False,
                        weight=float(c.get("weight", 1.0)),
                        machine_checkable=bool(c.get("machine_checkable", False)),
                    )
                    for c in raw.get("acceptance", [])
                ],
                anti_patterns=[str(p) for p in raw.get("anti_patterns", [])],
                evidence_required=[str(e) for e in raw.get("evidence_required", [])],
            )
        except Exception:
            return VerificationContract(
                id="fallback",
                goal=fallback_goal,
                acceptance=[
                    AcceptanceCriterion(
                        id="complete",
                        text="Goal is satisfactorily completed",
                    )
                ],
            )
