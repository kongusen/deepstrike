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


class AgentSession:
    """Pythonic session handle backed by one shared SessionLog."""

    def __init__(self, agent: "Agent", session_id: str) -> None:
        self.agent = agent
        self.id = session_id
        self._session_log = agent._session_log
        self._active_runner = None

    def _runner(self, goal: str):
        return self.agent._create_runner(self._session_log, goal=goal, session_id=self.id)

    async def stream(self, goal: str, *, max_turns: int | None = None):
        runner = self.agent._create_runner(self._session_log, goal=goal, session_id=self.id, max_turns=max_turns)
        self._active_runner = runner
        try:
            async for event in runner.run(goal=goal, session_id=self.id):
                yield event
        finally:
            self._active_runner = None

    async def resume(self):
        if self._active_runner is None:
            entries = await self._session_log.read(self.id)
            started = next(
                (entry.event for entry in entries if entry.event.get("kind") == "run_started"),
                None,
            )
            if started is None:
                raise ValueError(f"No run_started event for session: {self.id}")
            self._active_runner = self.agent._create_runner(
                self._session_log,
                goal=str(started.get("goal", "")),
                session_id=self.id,
            )
        try:
            async for event in self._active_runner.wake(self.id):
                yield event
        finally:
            self._active_runner = None

    def interrupt(self, reason: str = "user") -> None:
        if self._active_runner is not None:
            self._active_runner.interrupt(reason)

    async def run(self, goal: str, *, max_turns: int | None = None) -> str:
        from deepstrike.runtime.runner import collect_text
        return await collect_text(self.stream(goal, max_turns=max_turns))

    async def workflow(self, spec: Any):
        """Run a WorkflowSpec under this session's Kernel owner."""
        runner = self.agent._create_runner(self._session_log, goal="workflow", session_id=self.id)
        self._active_runner = runner
        try:
            return await runner.run_workflow(spec, session_id=self.id)
        finally:
            self._active_runner = None


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
        from deepstrike.runtime.session_log import FileSessionLog, InMemorySessionLog
        binding = self.runtime_binding or {}
        configured_log = binding.get("session_log")
        if configured_log is None and binding.get("session_log_dir") is not None:
            configured_log = FileSessionLog(binding["session_log_dir"])
        self._session_log = configured_log or InMemorySessionLog()
        self._sessions: dict[str, AgentSession] = {}

    @property
    def declaration(self) -> dict[str, Any]:
        """JSON-safe declaration snapshot; host callables remain private bindings."""
        return self._captured.declaration.to_kernel_dict()

    def session(self, session_id: str | None = None) -> AgentSession:
        resolved = session_id or f"agent-{uuid.uuid4()}"
        if resolved not in self._sessions:
            self._sessions[resolved] = AgentSession(self, resolved)
        return self._sessions[resolved]

    def _provider(self) -> Any:
        if not self.runtime_binding:
            raise RuntimeError(f'agent "{self.name}" has no runtime binding')
        provider = self.runtime_binding.get("provider")
        if provider is None:
            provider_for = self.runtime_binding.get("provider_for")
            provider = provider_for(self.model) if callable(provider_for) else None
        if provider is None:
            raise RuntimeError(f'agent "{self.name}" has no runtime provider binding')
        return provider

    def _create_runner(self, session_log: Any, *, goal: str, session_id: str, max_turns: int | None = None):
        from deepstrike.runtime.execution_plane import LocalExecutionPlane
        from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner

        binding = self.runtime_binding or {}
        options = dict(binding.get("runtime_options", {}))
        plane = options.pop("execution_plane", None)
        options.pop("session_log", None)
        if max_turns is not None:
            options["max_turns"] = max_turns
        options.setdefault("run_spec", self._captured.declaration.to_run_spec(goal=goal, session_id=session_id))
        if plane is None:
            plane = LocalExecutionPlane()
            if self._captured.host_tools:
                plane.register(*self._captured.host_tools)
        return RuntimeRunner(RuntimeOptions(
            provider=self._provider(),
            execution_plane=plane,
            session_log=session_log,
            agent_id=self.name,
            **({"system_prompt": self.instructions} if self.instructions else {}),
            **options,
        ))

    async def workflow(self, spec: Any, *, session_id: str | None = None):
        """Run a declarative workflow through the shared Kernel path."""
        return await self.session(session_id).workflow(spec)

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
        resolved_session_id = session_id or f"agent-{uuid.uuid4()}"
        output = await self.session(resolved_session_id).run(goal, max_turns=max_turns)
        return {"output": output, "status": "completed", "session_id": resolved_session_id}

    async def stream(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None):
        """Stream host events for the same public Agent contract."""
        resolved_session_id = session_id or f"agent-{uuid.uuid4()}"
        async for event in self.session(resolved_session_id).stream(goal, max_turns=max_turns):
            yield event


def create_agent(name: str, **kwargs: Any) -> Agent:
    """Create the executable public Agent handle."""
    return Agent(name, **kwargs)
