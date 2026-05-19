from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Literal

from deepstrike.collaboration.contract import (
    VerificationContract,
    format_contract_for_system_prompt,
)
from deepstrike.runtime import RuntimeRunner, collect_text

AgentRole = Literal["orchestrator", "executor", "verifier"]


@dataclass
class IsolatedVerifierContext:
    contract: VerificationContract
    artifact: str


class AgentPool:
    """
    Manages a set of role-specific RuntimeRunner instances.

    Each role runs in its own runner with an independent session log partition,
    ensuring that the verifier never sees the executor's implementation transcript.

    Usage::

        pool = (AgentPool()
            .add("executor", executor_runner)
            .add("verifier", verifier_runner))
    """

    def __init__(self) -> None:
        self._runners: dict[AgentRole, RuntimeRunner] = {}

    def add(self, role: AgentRole, runner: RuntimeRunner) -> "AgentPool":
        self._runners[role] = runner
        return self

    def has(self, role: AgentRole) -> bool:
        return role in self._runners

    def get(self, role: AgentRole) -> RuntimeRunner:
        runner = self._runners.get(role)
        if runner is None:
            raise ValueError(f'AgentPool: no runner registered for role "{role}"')
        return runner

    async def run_verifier(self, ctx: IsolatedVerifierContext) -> str:
        """
        Run the verifier with an isolated context.

        The verifier receives only the artifact + contract. It does NOT
        receive the executor's conversation history.
        """
        runner = self.get("verifier")
        contract_block = format_contract_for_system_prompt(ctx.contract)
        audit_goal = "\n".join([
            contract_block,
            "",
            "---",
            "",
            "## Artifact to Audit",
            "",
            ctx.artifact,
            "",
            "---",
            "",
            "Audit the artifact against every criterion in the contract above.",
            "For each criterion, state whether it PASSED or FAILED and cite specific evidence.",
            "List any anti-patterns you detected.",
            "Conclude with an overall PASS or FAIL verdict.",
        ])
        return await collect_text(runner.run(
            session_id=str(uuid.uuid4()),
            goal=audit_goal,
        ))

    async def run_orchestrator(self, goal: str) -> str:
        """
        Run the orchestrator to decompose a raw goal into a VerificationContract (JSON).
        """
        runner = self.get("orchestrator")
        orchestrator_goal = "\n".join([
            "You are a planning orchestrator. Decompose the following goal into a VerificationContract.",
            "",
            f"Goal: {goal}",
            "",
            "Produce a JSON object with this schema:",
            "{",
            '  "id": "<kebab-case-id>",',
            '  "goal": "<restated goal>",',
            '  "acceptance": [{ "id": "<id>", "text": "<criterion>", "required": true, "weight": 0.x, "machine_checkable": false }],',
            '  "anti_patterns": ["<pattern>"],',
            '  "evidence_required": ["<evidence item>"]',
            "}",
            "",
            "Output ONLY the JSON object, no prose.",
        ])
        return await collect_text(runner.run(
            session_id=str(uuid.uuid4()),
            goal=orchestrator_goal,
        ))
