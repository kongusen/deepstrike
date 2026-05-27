from __future__ import annotations

import uuid
from dataclasses import dataclass
from typing import Literal

from deepstrike.collaboration.contract import (
    VerificationContract,
    format_contract_for_system_prompt,
)
from deepstrike.runtime import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.runtime.sub_agent_orchestrator import spawn_standalone
from deepstrike.types.agent import AgentRunSpec, KernelAgentRole, SubAgentResult, agent_identity_sub

AgentRole = Literal["orchestrator", "executor", "verifier"]

KERNEL_ROLE_MAP: dict[AgentRole, KernelAgentRole] = {
    "orchestrator": "plan",
    "executor": "implement",
    "verifier": "verify",
}


@dataclass
class IsolatedVerifierContext:
    contract: VerificationContract
    artifact: str


@dataclass
class CoordinatorConfig:
    opts: RuntimeOptions
    session_id: str


class AgentPool:
    """
    Manages role-specific runners and optional kernel spawn coordination.

    When ``configure_coordinator()`` is set, ``spawn()`` uses the kernel
    sub-agent isolation path with parent-child lineage in the session log.
    """

    def __init__(self) -> None:
        self._runners: dict[AgentRole, RuntimeRunner] = {}
        self._coordinator: CoordinatorConfig | None = None

    def add(self, role: AgentRole, runner: RuntimeRunner) -> "AgentPool":
        self._runners[role] = runner
        return self

    def configure_coordinator(self, opts: RuntimeOptions, session_id: str) -> "AgentPool":
        self._coordinator = CoordinatorConfig(opts=opts, session_id=session_id)
        return self

    def ensure_coordinator(self, session_id: str | None = None) -> "AgentPool":
        """Infer coordinator from executor → orchestrator → verifier. Idempotent."""
        if self._coordinator is not None:
            return self
        source: AgentRole = (
            "executor" if self.has("executor")
            else "orchestrator" if self.has("orchestrator")
            else "verifier"
        )
        sid = session_id or str(uuid.uuid4())
        return self.configure_coordinator(self.get(source).host_options, sid)

    def uses_spawn_path(self) -> bool:
        return self._coordinator is not None

    def has(self, role: AgentRole) -> bool:
        return role in self._runners

    def get(self, role: AgentRole) -> RuntimeRunner:
        runner = self._runners.get(role)
        if runner is None:
            raise ValueError(f'AgentPool: no runner registered for role "{role}"')
        return runner

    async def spawn(
        self,
        role: AgentRole | KernelAgentRole,
        goal: str,
        **extra,
    ) -> SubAgentResult:
        if self._coordinator is None:
            raise RuntimeError("AgentPool.configure_coordinator() required for kernel spawn path")
        kernel_role: KernelAgentRole = (
            KERNEL_ROLE_MAP[role] if role in KERNEL_ROLE_MAP else role  # type: ignore[arg-type]
        )
        spec = AgentRunSpec(
            identity=agent_identity_sub(
                f"{kernel_role}-{uuid.uuid4()}",
                str(uuid.uuid4()),
                self._coordinator.session_id,
            ),
            role=kernel_role,
            goal=goal,
            **extra,
        )
        return await spawn_standalone(
            self._coordinator.opts,
            self._coordinator.session_id,
            spec,
        )

    async def run_verifier(self, ctx: IsolatedVerifierContext) -> str:
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
        if self._coordinator is not None:
            result = await self.spawn("verify", audit_goal, verification_contract_id=ctx.contract.id, isolation="read_only")
            final = result.result.final_message
            return getattr(final, "content", "") if final else ""
        return await collect_text(self.get("verifier").run(
            session_id=str(uuid.uuid4()),
            goal=audit_goal,
        ))

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
        if self._coordinator is not None:
            result = await self.spawn("plan", orchestrator_goal)
            final = result.result.final_message
            return getattr(final, "content", "") if final else ""
        return await collect_text(self.get("orchestrator").run(
            session_id=str(uuid.uuid4()),
            goal=orchestrator_goal,
        ))
