from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Callable, Optional, TYPE_CHECKING

from deepstrike.collaboration.contract import (
    VerificationContract,
    ContractCheckResult,
    format_contract_for_system_prompt,
    contract_to_criteria_strings,
)
from deepstrike.collaboration.handoff import HandoffArtifact, HandoffBus, ContractOutcomeInput

if TYPE_CHECKING:
    from deepstrike.collaboration.pool import AgentPool


@dataclass
class Violation:
    criterion_id: str
    text: str
    detail: str


@dataclass
class ContractOutcome:
    success: bool
    artifact: str
    check_results: list[ContractCheckResult]
    attempts_used: int
    total_tokens_consumed: int
    handoff: HandoffArtifact


@dataclass
class ContractHarnessOptions:
    max_attempts: int = 3
    on_violation: Optional[Callable[[list[Violation]], None]] = None


class ContractDrivenHarness:
    """
    Core multi-agent execution primitive.

    Differs from HarnessLoop in three ways:
      1. Executor and verifier are separate Agent instances — no shared history.
      2. Verifier receives only the artifact + contract, not the implementation transcript.
      3. Feedback is a structured Violation list, not free-text.

    Protocol per attempt:
      executor.run(goal, contract) → artifact
      verifier.run_isolated(artifact, contract) → audit text
      parse audit → ContractCheckResult[]
      all required pass → Done
      violations remain → inject violation list into next executor goal
    """

    def __init__(
        self,
        pool: "AgentPool",
        contract: VerificationContract,
        options: Optional[ContractHarnessOptions] = None,
    ) -> None:
        self._pool = pool
        self._contract = contract
        self._opts = options or ContractHarnessOptions()

    async def run(self) -> ContractOutcome:
        from deepstrike.collaboration.pool import IsolatedVerifierContext

        artifact = ""
        check_results: list[ContractCheckResult] = []
        attempts_used = 0
        current_goal = self._contract.goal

        for attempt in range(1, self._opts.max_attempts + 1):
            attempts_used = attempt

            # Phase 1: Executor — sees contract + goal only
            contract_block = format_contract_for_system_prompt(self._contract)
            violation_note = ""
            if attempt > 1:
                violation_note = (
                    "\n\n[Previous attempt failed. Violations to fix:\n"
                    + self._format_violations_for_feedback(check_results)
                    + "]"
                )
            executor_goal = f"{contract_block}\n\n---\n\n{current_goal}{violation_note}"

            artifact = await self._pool.get("executor").run(
                executor_goal,
                contract_to_criteria_strings(self._contract),
            )

            # Phase 2: Verifier — isolated context, no executor history
            audit_text = await self._pool.run_verifier(
                IsolatedVerifierContext(contract=self._contract, artifact=artifact)
            )

            # Phase 3: Parse audit
            check_results = self._parse_audit_text(audit_text)
            violations = self._find_violations(check_results)

            if not violations:
                return ContractOutcome(
                    success=True,
                    artifact=artifact,
                    check_results=check_results,
                    attempts_used=attempts_used,
                    total_tokens_consumed=0,
                    handoff=HandoffBus.from_contract_outcome(
                        ContractOutcomeInput(
                            contract=self._contract,
                            check_results=check_results,
                            artifact=artifact,
                            success=True,
                        )
                    ),
                )

            if self._opts.on_violation:
                self._opts.on_violation(violations)

        blocked_on = [
            f"[{v.criterion_id}] {v.text}: {v.detail}"
            for v in self._find_violations(check_results)
        ]
        return ContractOutcome(
            success=False,
            artifact=artifact,
            check_results=check_results,
            attempts_used=attempts_used,
            total_tokens_consumed=0,
            handoff=HandoffBus.from_contract_outcome(
                ContractOutcomeInput(
                    contract=self._contract,
                    check_results=check_results,
                    artifact=artifact,
                    success=False,
                    blocked_on=blocked_on,
                )
            ),
        )

    def _find_violations(self, results: list[ContractCheckResult]) -> list[Violation]:
        violations = []
        for result in results:
            if not result.passed:
                criterion = next(
                    (c for c in self._contract.acceptance if c.id == result.criterion_id), None
                )
                if criterion and criterion.required:
                    violations.append(Violation(
                        criterion_id=result.criterion_id,
                        text=criterion.text,
                        detail=result.evidence or "no evidence provided",
                    ))
        return violations

    def _format_violations_for_feedback(self, results: list[ContractCheckResult]) -> str:
        return "\n".join(
            f"- [{v.criterion_id}] {v.text}: {v.detail}"
            for v in self._find_violations(results)
        )

    def _parse_audit_text(self, audit_text: str) -> list[ContractCheckResult]:
        results = []
        lower = audit_text.lower()

        for criterion in self._contract.acceptance:
            escaped = re.escape(criterion.id)
            pattern = re.compile(rf"\b{escaped}\b[^\n]*?(pass|fail)", re.IGNORECASE)
            match = pattern.search(audit_text)

            if match:
                passed = match.group(1).lower() == "pass"
                line_start = audit_text.rfind("\n", 0, match.start()) + 1
                line_end = audit_text.find("\n", match.start())
                evidence = audit_text[line_start: line_end if line_end > 0 else None].strip()
                results.append(ContractCheckResult(
                    criterion_id=criterion.id,
                    passed=passed,
                    evidence=evidence,
                ))
            else:
                snippet = criterion.text.lower()[:30]
                text_idx = lower.find(snippet)
                if text_idx != -1:
                    window = lower[text_idx: text_idx + 200]
                    passed = "pass" in window and "fail" not in window
                    results.append(ContractCheckResult(
                        criterion_id=criterion.id,
                        passed=passed,
                        evidence="inferred from context",
                    ))
                else:
                    results.append(ContractCheckResult(
                        criterion_id=criterion.id,
                        passed=False,
                        evidence="criterion not mentioned in audit",
                    ))

        return results
