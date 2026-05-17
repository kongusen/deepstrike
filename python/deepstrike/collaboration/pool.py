from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

from deepstrike.collaboration.contract import (
    VerificationContract,
    format_contract_for_system_prompt,
    contract_to_criteria_strings,
)

if TYPE_CHECKING:
    from deepstrike.agent import Agent

AgentRole = Literal["orchestrator", "executor", "verifier"]


@dataclass
class IsolatedVerifierContext:
    contract: VerificationContract
    artifact: str


class AgentPool:
    """
    Manages a set of role-specific Agent instances.

    Each role runs in its own Agent instance with an independent history partition,
    ensuring that the verifier never sees the executor's implementation transcript.

    Usage::

        pool = (AgentPool()
            .add("executor", executor_agent)
            .add("verifier", verifier_agent))
    """

    def __init__(self) -> None:
        self._agents: dict[AgentRole, "Agent"] = {}

    def add(self, role: AgentRole, agent: "Agent") -> "AgentPool":
        self._agents[role] = agent
        return self

    def has(self, role: AgentRole) -> bool:
        return role in self._agents

    def get(self, role: AgentRole) -> "Agent":
        agent = self._agents.get(role)
        if agent is None:
            raise ValueError(f"AgentPool: no agent registered for role '{role}'")
        return agent

    async def run_verifier(self, ctx: IsolatedVerifierContext) -> str:
        """
        Run the verifier with an isolated context.

        The verifier receives only the artifact + contract. It does NOT
        receive the executor's conversation history.
        """
        agent = self.get("verifier")
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
        return await agent.run(audit_goal)

    async def run_orchestrator(self, goal: str) -> str:
        """
        Run the orchestrator to decompose a raw goal into a VerificationContract (JSON).
        """
        agent = self.get("orchestrator")
        orchestrator_goal = "\n".join([
            "You are a planning orchestrator. Decompose the following goal into a VerificationContract.",
            "",
            f"Goal: {goal}",
            "",
            "Produce a JSON object with this schema:",
            '{',
            '  "id": "<kebab-case-id>",',
            '  "goal": "<restated goal>",',
            '  "acceptance": [{ "id": "<id>", "text": "<criterion>", "required": true, "weight": 0.x, "machine_checkable": false }],',
            '  "anti_patterns": ["<pattern>"],',
            '  "evidence_required": ["<evidence item>"]',
            '}',
            "",
            "Output ONLY the JSON object, no prose.",
        ])
        return await agent.run(orchestrator_goal)
