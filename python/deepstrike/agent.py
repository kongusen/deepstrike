"""Public executable Agent contract.

This surface describes an agent before a host chooses a provider, execution plane, or Kernel run.
The declaration stays provider-neutral; execution resolves a provider through the explicit
``runtime_binding`` supplied by the host.
"""
from __future__ import annotations

from dataclasses import dataclass
import uuid
from typing import Any, Literal, Mapping, Sequence, TypeAlias

from deepstrike.types.agent import AgentCapabilityFilter
from deepstrike.runtime.agent_declaration import capture_agent_declaration, CapturedAgent


ModelRef: TypeAlias = str | dict[str, Any]
AgentDefinition: TypeAlias = Mapping[str, Any]
AgentMemory: TypeAlias = Any


@dataclass(frozen=True)
class MemoryReference:
    """Serializable durable-memory binding for an agent declared before a host store exists."""

    namespace: str | None = None
    kind: Literal["durable"] = "durable"


class Agent:
    """Provider-neutral declaration and executable public Agent handle.

    ``tools`` may contain executable ``RegisteredTool`` instances or JSON-safe tool descriptors.
    The latter carry schema only and do not create executable capabilities.
    """

    def __init__(
        self,
        name: str,
        *,
        description: str | None = None,
        instructions: str | None = None,
        model: ModelRef | None = None,
        capability_filter: AgentCapabilityFilter | Mapping[str, Any] | None = None,
        tools: Sequence[Any] | None = None,
        mcp_servers: Sequence[Mapping[str, Any]] | None = None,
        skills: Sequence[Mapping[str, Any]] | None = None,
        memory: AgentMemory | None = None,
        knowledge: Sequence[Mapping[str, Any]] | None = None,
        handoffs: Sequence[Mapping[str, Any]] | None = None,
        provider_options: Mapping[str, Any] | None = None,
        output_schema: Mapping[str, Any] | None = None,
        metadata: Mapping[str, Any] | None = None,
        guardrails: Sequence[Mapping[str, Any]] | None = None,
        runtime_binding: Mapping[str, Any] | None = None,
    ) -> None:
        captured = capture_agent_declaration(
            name,
            description=description,
            instructions=instructions,
            model=model,
            capability_filter=capability_filter,
            tools=tools,
            mcp_servers=mcp_servers,
            skills=skills,
            memory=memory,
            knowledge=knowledge,
            handoffs=handoffs,
            provider_options=provider_options,
            output_schema=output_schema,
            metadata=metadata,
            guardrails=guardrails,
            runtime_binding=runtime_binding,
        )
        self._captured: CapturedAgent = captured
        self.name = name
        self.description = description
        self.instructions = instructions
        self.model = model
        self.capability_filter = capability_filter
        self.tools = list(tools) if tools is not None else None
        self.mcp_servers = list(mcp_servers) if mcp_servers is not None else None
        self.skills = list(skills) if skills is not None else None
        self.memory = memory
        self.knowledge = list(knowledge) if knowledge is not None else None
        self.handoffs = list(handoffs) if handoffs is not None else None
        self.provider_options = dict(provider_options) if provider_options is not None else None
        self.output_schema = dict(output_schema) if output_schema is not None else None
        self.metadata = dict(metadata) if metadata is not None else None
        self.guardrails = list(guardrails) if guardrails is not None else None
        self.runtime_binding = dict(runtime_binding) if runtime_binding is not None else None

    @property
    def declaration(self) -> dict[str, Any]:
        """JSON-safe declaration snapshot; host callables remain private bindings."""
        return self._captured.declaration.to_kernel_dict()

    async def run(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None) -> dict[str, Any]:
        """Execute one goal through the host binding and return a structured run result."""
        if not self.runtime_binding:
            raise RuntimeError(f'agent "{self.name}" has no runtime binding')
        provider = self.runtime_binding.get("provider")
        if provider is None:
            provider_for = self.runtime_binding.get("provider_for")
            provider = provider_for(self.model) if callable(provider_for) else None
        if provider is None:
            raise RuntimeError(f'agent "{self.name}" has no runtime provider binding')
        from deepstrike.runtime.facade import run_agent
        resolved_session_id = session_id or f"agent-{uuid.uuid4()}"
        output = await run_agent(
            provider=provider,
            goal=goal,
            system_prompt=self.instructions,
            tools=list(self._captured.host_tools),
            session_id=resolved_session_id,
            max_turns=max_turns,
            run_spec=self._captured.declaration.to_run_spec(
                goal=goal,
                session_id=resolved_session_id,
            ),
        )
        return {"output": output, "status": "completed", "session_id": session_id}

    async def stream(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None):
        """Stream host events for the same public Agent contract."""
        if not self.runtime_binding:
            raise RuntimeError(f'agent "{self.name}" has no runtime binding')
        provider = self.runtime_binding.get("provider")
        if provider is None:
            provider_for = self.runtime_binding.get("provider_for")
            provider = provider_for(self.model) if callable(provider_for) else None
        if provider is None:
            raise RuntimeError(f'agent "{self.name}" has no runtime provider binding')
        from deepstrike.runtime.execution_plane import LocalExecutionPlane
        from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner
        from deepstrike.runtime.session_log import InMemorySessionLog
        options = dict(self.runtime_binding.get("runtime_options", {}))
        plane = options.pop("execution_plane", None) if isinstance(options, dict) else None
        resolved_session_id = session_id or f"agent-{uuid.uuid4()}"
        options.setdefault(
            "run_spec",
            self._captured.declaration.to_run_spec(goal=goal, session_id=resolved_session_id),
        )
        runner = RuntimeRunner(RuntimeOptions(
            provider=provider,
            execution_plane=plane or LocalExecutionPlane(),
            session_log=InMemorySessionLog(),
            max_tokens=32_000,
            agent_id=self.name,
            **({"system_prompt": self.instructions} if self.instructions else {}),
            **({"max_turns": max_turns} if max_turns is not None else {}),
            **options,
        ))
        async for event in runner.run(goal=goal, session_id=resolved_session_id):
            yield event


def create_agent(name: str, **kwargs: Any) -> Agent:
    """Create the executable public Agent handle."""
    return Agent(name, **kwargs)
