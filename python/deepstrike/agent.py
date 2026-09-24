"""Public executable Agent contract.

This surface describes an agent before a host chooses a provider, execution plane, or Kernel run.
The declaration stays provider-neutral; execution resolves a provider through the explicit
``runtime_binding`` supplied by the host.
"""
from __future__ import annotations

from dataclasses import dataclass
import uuid
import time
from collections.abc import AsyncIterator
from typing import Any, Literal, Mapping, Sequence, TypeAlias

from deepstrike.types.agent import AgentCapabilityFilter


ModelRef: TypeAlias = str | dict[str, Any]
AgentDefinition: TypeAlias = Mapping[str, Any]
AgentMemory: TypeAlias = Any


@dataclass(frozen=True)
class MemoryReference:
    """Serializable durable-memory binding for an agent declared before a host store exists."""

    namespace: str | None = None
    kind: Literal["durable"] = "durable"


class RunResult(dict[str, Any]):
    """Completed run data with both mapping and attribute-style access.

    Mapping access keeps the 0.2.x facade compatible while attributes make the
    result pleasant to use in normal Python code.
    """

    def __init__(
        self,
        *,
        output: str,
        run_id: str | None,
        session_id: str,
        status: str,
        usage: dict[str, Any] | None = None,
        evidence: list[dict[str, Any]] | None = None,
    ) -> None:
        super().__init__(
            output=output,
            run_id=run_id,
            session_id=session_id,
            status=status,
            usage=usage,
            evidence=evidence or [],
        )

    output: str
    run_id: str | None
    session_id: str
    status: str
    usage: dict[str, Any] | None
    evidence: list[dict[str, Any]]

    def __getattr__(self, name: str) -> Any:
        try:
            return self[name]
        except KeyError as exc:
            raise AttributeError(name) from exc


class AgentSession:
    """A reusable, resumable session owned by one :class:`Agent`."""

    def __init__(self, agent: "Agent", session_id: str) -> None:
        self.agent = agent
        self.id = session_id
        self._runner = agent._runner_for(session_id)

    async def stream(
        self,
        goal: str,
        *,
        max_turns: int | None = None,
    ) -> AsyncIterator[Any]:
        runner = self._runner if max_turns is None else self.agent._runner_for(self.id, max_turns=max_turns)
        async for event in runner.run(goal=goal, session_id=self.id):
            yield event

    async def resume(self) -> AsyncIterator[Any]:
        async for event in self._runner.wake(self.id):
            yield event

    def interrupt(self, reason: str = "user") -> None:
        self._runner.interrupt(reason)  # type: ignore[arg-type]

    async def run(self, goal: str, *, max_turns: int | None = None) -> RunResult:
        return await self.agent._run_in_session(self, goal, max_turns=max_turns)


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
        if not name:
            raise ValueError("agent name is required")
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
        self._session_log = None
        self._runners: dict[str, Any] = {}

    @property
    def declaration(self) -> dict[str, Any]:
        """Return the serializable agent declaration used by runtime adapters."""
        return {
            "name": self.name,
            "description": self.description,
            "instructions": self.instructions,
            "model": self.model,
            "capability_filter": self.capability_filter,
            "tools": self.tools,
            "mcp_servers": self.mcp_servers,
            "skills": self.skills,
            "memory": self.memory,
            "knowledge": self.knowledge,
            "handoffs": self.handoffs,
            "provider_options": self.provider_options,
            "output_schema": self.output_schema,
            "metadata": self.metadata,
            "guardrails": self.guardrails,
        }

    def session(self, session_id: str | None = None) -> AgentSession:
        """Return a stable session handle, creating an id when omitted."""
        return AgentSession(self, session_id or f"agent-{uuid.uuid4()}")

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

    def _runner_for(self, session_id: str, *, max_turns: int | None = None) -> Any:
        runner = self._runners.get(session_id)
        if runner is not None and max_turns is None:
            return runner
        from deepstrike.runtime.execution_plane import LocalExecutionPlane
        from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner
        from deepstrike.runtime.session_log import InMemorySessionLog

        if self._session_log is None:
            self._session_log = self.runtime_binding.get("session_log") if self.runtime_binding else None
            if self._session_log is None:
                self._session_log = InMemorySessionLog()
        raw_options = dict((self.runtime_binding or {}).get("runtime_options") or {})
        raw_options.pop("provider", None)
        raw_options.pop("session_log", None)
        raw_options.pop("execution_plane", None)
        raw_options.pop("agent_id", None)
        raw_options.pop("system_prompt", None)
        if max_turns is not None:
            raw_options["max_turns"] = max_turns
        binding = self.runtime_binding or {}
        if binding.get("memory_store") is not None:
            raw_options.setdefault("memory_store", binding["memory_store"])
        if binding.get("memory_scope") is not None:
            raw_options.setdefault("memory_scope", binding["memory_scope"])
        configured_plane = (self.runtime_binding or {}).get("execution_plane")
        plane = configured_plane or LocalExecutionPlane()
        if configured_plane is None:
            executable_tools = [tool for tool in (self.tools or []) if hasattr(tool, "schema")]
            if executable_tools:
                plane.register(*executable_tools)
        options = RuntimeOptions(
            provider=self._provider(),
            execution_plane=plane,
            session_log=self._session_log,
            agent_id=self.name,
            **({"system_prompt": self.instructions} if self.instructions else {}),
            **raw_options,
        )
        runner = RuntimeRunner(options)
        if max_turns is None:
            self._runners[session_id] = runner
        return runner

    def _memory_binding(self) -> tuple[Any, Any]:
        binding = self.runtime_binding or {}
        store = binding.get("memory_store")
        scope = binding.get("memory_scope")
        if store is None or scope is None:
            raise RuntimeError(
                f'agent "{self.name}" memory APIs require runtime_binding.memory_store and memory_scope'
            )
        return store, scope

    async def remember(
        self,
        memory: Any,
        *,
        session_id: str | None = None,
        name: str | None = None,
        kind: str = "reference",
        description: str = "",
        pinned: bool = False,
    ) -> Any:
        """Persist one durable memory through the same runner/kernel path as a live run."""
        from deepstrike.memory.protocols import MemoryProvenance, MemoryRecord

        _store, scope = self._memory_binding()
        if isinstance(memory, MemoryRecord):
            record = memory
        else:
            content = str(memory)
            now = int(time.time() * 1000)
            record = MemoryRecord(
                record_id=f"mem-{uuid.uuid4()}",
                scope=scope,
                name=name or "memory",
                kind=kind,  # type: ignore[arg-type]
                content=content,
                description=description,
                provenance=MemoryProvenance(author="host", trust="user_asserted", session_id=session_id),
                created_at=now,
                updated_at=now,
                pinned=pinned,
            )
        sid = session_id or f"agent-memory-{uuid.uuid4()}"
        runner = self._runner_for(sid)
        await runner.write_memory(record, session_id=sid, agent_id=self.name)
        return record

    async def recall(
        self,
        query: str,
        *,
        session_id: str | None = None,
        top_k: int = 5,
        min_score: float | None = None,
    ) -> list[Any]:
        """Search durable memory using the configured store and scope."""
        _store, scope = self._memory_binding()
        from deepstrike.memory.protocols import MemoryQuery

        sid = session_id or f"agent-memory-{uuid.uuid4()}"
        runner = self._runner_for(sid)
        return await runner.query_memory(
            MemoryQuery(scope=scope, query=query, top_k=top_k, min_score=min_score),
            session_id=sid,
            agent_id=self.name,
        )

    async def workflow(self, spec: Any, *, session_id: str | None = None) -> Any:
        """Run a declarative workflow through the runner's governed workflow path."""
        sid = session_id or f"agent-workflow-{uuid.uuid4()}"
        return await self._runner_for(sid).run_workflow(spec, session_id=sid)

    async def _run_in_session(self, session: AgentSession, goal: str, *, max_turns: int | None = None) -> RunResult:
        from deepstrike.providers.stream import DoneEvent, ErrorEvent, TextDelta, UsageEvent

        output: list[str] = []
        usage: dict[str, Any] | None = None
        status = "completed"
        async for event in session.stream(goal, max_turns=max_turns):
            if isinstance(event, TextDelta):
                output.append(event.delta)
            elif isinstance(event, UsageEvent):
                usage = {
                    "input_tokens": event.input_tokens,
                    "output_tokens": event.output_tokens,
                    "total_tokens": event.total_tokens,
                }
            elif isinstance(event, DoneEvent):
                status = event.status
            elif isinstance(event, ErrorEvent):
                status = "error"
        entries = await self._session_log.read(session.id) if self._session_log is not None else []
        started = next((entry.event for entry in entries if entry.event.get("kind") == "run_started"), None)
        evidence = [entry.event for entry in entries if entry.event.get("kind") in {"provider_attempt", "llm_completed"}]
        return RunResult(
            output="".join(output),
            run_id=started.get("run_id") if started else None,
            session_id=session.id,
            status=status,
            usage=usage,
            evidence=evidence,
        )

    async def run(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None) -> RunResult:
        """Execute one goal and return a reusable, structured result."""
        session = self.session(session_id)
        return await self._run_in_session(session, goal, max_turns=max_turns)

    async def stream(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None):
        """Stream host events while retaining the session for later resume."""
        session = self.session(session_id)
        async for event in session.stream(goal, max_turns=max_turns):
            yield event

    def close(self) -> None:
        """Interrupt active runs owned by this agent."""
        for runner in self._runners.values():
            runner.interrupt("host_shutdown")


def create_agent(name: str, **kwargs: Any) -> Agent:
    """Create the executable public Agent handle."""
    return Agent(name, **kwargs)
