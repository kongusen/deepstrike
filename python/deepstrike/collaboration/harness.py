"""Creator/verifier policies consumed by the shared AttemptLoop engine."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING

from deepstrike._kernel import parse_verdict
from deepstrike.collaboration.contract import ContractCheckResult, VerificationContract
from deepstrike.collaboration.contract import format_contract_for_system_prompt
from deepstrike.collaboration.handoff import HandoffArtifact
from deepstrike.harness.harness import (
    AttemptBodyContext,
    AttemptBodyEvent,
    AttemptBodyTerminal,
    AttemptProgressEvent,
    CriterionResult,
    Verdict,
)
from deepstrike.harness.judge import JudgeContext, JudgeResult

if TYPE_CHECKING:
    from deepstrike.collaboration.pool import AgentPool


@dataclass
class ContractOutcome:
    success: bool
    artifact: str
    check_results: list[ContractCheckResult]
    attempts_used: int
    total_tokens_consumed: int
    handoff: HandoffArtifact


class CreatorVerifierBody:
    """Execution-only policy; verification belongs to StructuredContractJudge."""

    def __init__(self, pool: "AgentPool", contract: VerificationContract) -> None:
        self._pool = pool
        self._contract = contract

    async def run(self, context: AttemptBodyContext):
        contract_block = format_contract_for_system_prompt(self._contract)
        result = await self._pool.execute(
            "executor",
            session_id=context.session_id,
            goal=f"{contract_block}\n\n---\n\n{context.goal}",
            context_input=context.context_input,
            verification_contract_id=self._contract.id,
        )
        final = result.result.final_message
        artifact = str(getattr(final, "content", "")) if final else ""
        if artifact:
            yield AttemptProgressEvent("token", {"text": artifact})
        yield AttemptBodyTerminal(
            run_status=result.result.termination,
            result=artifact,
            turns=result.result.turns_used,
            total_tokens=result.result.total_tokens_used,
            submitted_nodes=list(result.submitted_nodes),
        )


class StructuredContractJudge:
    """Accept only the shared structured verdict schema; free-text PASS/FAIL is invalid."""

    def __init__(self, pool: "AgentPool", contract: VerificationContract) -> None:
        self._pool = pool
        self._contract = contract

    async def judge(self, context: JudgeContext) -> JudgeResult:
        from deepstrike.collaboration.pool import IsolatedVerifierContext

        audit_text = await self._pool.run_verifier(
            IsolatedVerifierContext(contract=self._contract, artifact=context.result)
        )
        wire = json.loads(audit_text)
        if not isinstance(wire, dict):
            raise ValueError("structured verifier output must be a JSON object")
        parsed = parse_verdict(audit_text)

        parsed_details = list(parsed.details or [])
        details: list[CriterionResult] = []
        for criterion in self._contract.acceptance:
            match = next(
                (
                    detail
                    for detail in parsed_details
                    if detail.criterion in {criterion.id, criterion.text}
                ),
                None,
            )
            details.append(
                CriterionResult(
                    criterion=criterion.id,
                    passed=bool(match.passed) if match else False,
                    score=float(match.score) if match else 0.0,
                    feedback=(
                        str(match.feedback)
                        if match
                        else "criterion missing from structured verifier output"
                    ),
                )
            )

        required_passed = all(
            not criterion.required or details[index].passed
            for index, criterion in enumerate(self._contract.acceptance)
        )
        return JudgeResult(
            verdict=Verdict(
                passed=bool(parsed.passed) and required_passed,
                overall_score=float(parsed.overall_score),
                feedback=str(parsed.feedback),
                details=details,
            )
        )
