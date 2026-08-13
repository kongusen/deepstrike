from __future__ import annotations

import json
import uuid
from dataclasses import dataclass

from deepstrike.collaboration.contract import (
    VerificationContract,
    format_contract_for_system_prompt,
)
from deepstrike.runtime import RuntimeOptions
from deepstrike.runtime.sub_agent_orchestrator import spawn_standalone
from deepstrike.types.agent import (
    AgentRunSpec,
    KernelAgentRole,
    SubAgentResult,
    agent_identity_sub,
)

AgentRole = KernelAgentRole


@dataclass
class IsolatedVerifierContext:
    contract: VerificationContract
    artifact: str


@dataclass
class CoordinatorConfig:
    opts: RuntimeOptions
    session_id: str


class AgentPool:
    """Runs every collaboration role through one kernel coordinator."""

    def __init__(self) -> None:
        self._coordinator: CoordinatorConfig | None = None

    def configure_coordinator(self, opts: RuntimeOptions, session_id: str) -> "AgentPool":
        self._coordinator = CoordinatorConfig(opts=opts, session_id=session_id)
        return self

    def ensure_coordinator(self) -> "AgentPool":
        """Assert that the canonical kernel coordinator is configured."""
        if self._coordinator is None:
            raise RuntimeError("AgentPool.configure_coordinator() must be called before execution")
        return self

    def uses_spawn_path(self) -> bool:
        return self._coordinator is not None

    async def spawn(
        self,
        role: KernelAgentRole,
        goal: str,
        **extra,
    ) -> SubAgentResult:
        if self._coordinator is None:
            raise RuntimeError("AgentPool.configure_coordinator() required for kernel spawn path")
        spec = AgentRunSpec(
            identity=agent_identity_sub(
                f"{role}-{uuid.uuid4()}",
                str(uuid.uuid4()),
                self._coordinator.session_id,
            ),
            role=role,
            goal=goal,
            **extra,
        )
        return await spawn_standalone(
            self._coordinator.opts,
            self._coordinator.session_id,
            spec,
        )

    async def execute(
        self,
        role: KernelAgentRole,
        *,
        session_id: str,
        goal: str,
        context_input: str | None = None,
        verification_contract_id: str | None = None,
    ) -> SubAgentResult:
        """Run one body attempt in a caller-owned session."""

        self.ensure_coordinator()
        coordinator = self._coordinator
        assert coordinator is not None
        spec = AgentRunSpec(
            identity=agent_identity_sub(f"{role}-{uuid.uuid4()}", session_id, coordinator.session_id),
            role=role,
            goal=goal,
            verification_contract_id=verification_contract_id,
        )
        return await spawn_standalone(
            coordinator.opts,
            coordinator.session_id,
            spec,
            context_input=context_input,
        )

    async def run_verifier(self, ctx: IsolatedVerifierContext) -> str:
        from deepstrike._kernel import verdict_output_schema

        contract_block = format_contract_for_system_prompt(ctx.contract)
        schema = json.loads(verdict_output_schema(False))
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
            "Return only JSON matching this schema; free-text PASS/FAIL is invalid:",
            json.dumps(schema, ensure_ascii=False),
        ])
        result = await self.spawn("verify", audit_goal, verification_contract_id=ctx.contract.id, isolation="read_only")
        final = result.result.final_message
        return getattr(final, "content", "") if final else ""

    async def run_orchestrator(self, goal: str) -> str:
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
        result = await self.spawn("plan", orchestrator_goal)
        final = result.result.final_message
        return getattr(final, "content", "") if final else ""
