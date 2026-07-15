from __future__ import annotations

import json

import pytest

from deepstrike._kernel import Message
from deepstrike.collaboration.contract import AcceptanceCriterion, VerificationContract
from deepstrike.collaboration.modes import CreatorVerifierMode
from deepstrike.types.agent import LoopResult, SubAgentResult


class _Pool:
    def __init__(self) -> None:
        self.sessions: list[str] = []
        self.context_inputs: list[str | None] = []
        self.verifications = 0

    def ensure_coordinator(self, session_id=None):
        return self

    async def execute(
        self,
        role,
        *,
        session_id,
        goal,
        context_input=None,
        verification_contract_id=None,
    ):
        self.sessions.append(session_id)
        self.context_inputs.append(context_input)
        return SubAgentResult(
            agent_id="executor",
            result=LoopResult(
                termination="completed",
                turns_used=1,
                total_tokens_used=10,
                final_message=Message(role="assistant", content=f"artifact-{len(self.sessions)}"),
            ),
        )

    async def run_verifier(self, context):
        self.verifications += 1
        passed = self.verifications == 2
        return json.dumps({
            "passed": passed,
            "overall_score": 1 if passed else 0,
            "feedback": "ok" if passed else "fix evidence",
            "details": [{
                "criterion": "c1",
                "passed": passed,
                "score": 1 if passed else 0,
                "feedback": "good" if passed else "missing",
            }],
        })


@pytest.mark.asyncio
async def test_creator_verifier_uses_shared_attempt_loop_and_structured_judge():
    pool = _Pool()
    contract = VerificationContract(
        id="contract",
        goal="build it",
        acceptance=[AcceptanceCriterion(id="c1", text="has evidence")],
    )

    outcome = await CreatorVerifierMode(pool, max_attempts=2).run(contract)

    assert outcome.success is True
    assert outcome.artifact == "artifact-2"
    assert outcome.attempts_used == 2
    assert outcome.total_tokens_consumed == 20
    assert outcome.check_results[0].criterion_id == "c1"
    assert outcome.check_results[0].passed is True
    assert pool.sessions[0] == pool.sessions[1]
    assert pool.context_inputs == [None, "fix evidence"]


@pytest.mark.asyncio
async def test_structured_judge_rejects_free_text_verdict():
    pool = _Pool()

    async def free_text(_context):
        return "PASS"

    pool.run_verifier = free_text
    contract = VerificationContract(id="contract", goal="build it")

    with pytest.raises(json.JSONDecodeError):
        await CreatorVerifierMode(pool, max_attempts=1).run(contract)
