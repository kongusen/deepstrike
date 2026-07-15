from __future__ import annotations

import json
import uuid
from dataclasses import dataclass
from typing import Literal

from deepstrike.collaboration.contract import (
    VerificationContract,
    format_contract_for_system_prompt,
)
from deepstrike.runtime import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.runtime.sub_agent_orchestrator import spawn_standalone
from deepstrike.providers.stream import DoneEvent, TextDelta, WorkflowNodesSubmittedEvent
from deepstrike.types.agent import (
    AgentRunSpec,
    KernelAgentRole,
    LoopResult,
    SubAgentResult,
    agent_identity_sub,
)

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

    async def execute(
        self,
        role: AgentRole,
        *,
        session_id: str,
        goal: str,
        context_input: str | None = None,
        verification_contract_id: str | None = None,
    ) -> SubAgentResult:
        """Run one body attempt in a caller-owned session."""

        if self._coordinator is not None:
            kernel_role = KERNEL_ROLE_MAP[role]
            spec = AgentRunSpec(
                identity=agent_identity_sub(
                    f"{kernel_role}-{uuid.uuid4()}",
                    session_id,
                    self._coordinator.session_id,
                ),
                role=kernel_role,
                goal=goal,
                verification_contract_id=verification_contract_id,
            )
            return await spawn_standalone(
                self._coordinator.opts,
                self._coordinator.session_id,
                spec,
                context_input=context_input,
            )

        runner = self.get(role)
        if context_input:
            runner.inject_note(context_input)
        text = ""
        done: DoneEvent | None = None
        submitted_nodes: list = []
        async for event in runner.run(session_id=session_id, goal=goal):
            if isinstance(event, TextDelta):
                text += event.delta
            elif isinstance(event, DoneEvent):
                done = event
            elif isinstance(event, WorkflowNodesSubmittedEvent):
                submitted_nodes.extend(event.nodes)

        from deepstrike._kernel import Message

        return SubAgentResult(
            agent_id=f"{role}-{session_id}",
            result=LoopResult(
                termination=done.status if done else "error",
                turns_used=done.iterations if done else 0,
                total_tokens_used=done.total_tokens if done else 0,
                final_message=Message(role="assistant", content=text) if text else None,
            ),
            submitted_nodes=submitted_nodes,
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
