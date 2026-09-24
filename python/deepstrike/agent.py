"""Public executable Agent contract.

This surface describes an agent before a host chooses a provider, execution plane, or Kernel run.
The declaration stays provider-neutral; execution resolves a provider through the explicit
``runtime_binding`` supplied by the host.
"""
from __future__ import annotations

from dataclasses import dataclass
import uuid
import time
from typing import Any, Literal, Mapping, Sequence, TypeAlias

from deepstrike.types.agent import AgentCapabilityFilter
from deepstrike.runtime.agent_declaration import capture_agent_declaration, CapturedAgent


class _MemoryOnlyProvider:
    async def complete(self, *args: Any, **kwargs: Any):
        raise RuntimeError("memory-only agent cannot perform a model completion")

    async def stream(self, *args: Any, **kwargs: Any):
        raise RuntimeError("memory-only agent cannot perform a model stream")
        yield  # pragma: no cover


class AgentSession:
    """Pythonic session handle backed by one shared SessionLog."""

    def __init__(self, agent: "Agent", session_id: str) -> None:
        self.agent = agent
        self.id = session_id
        self._session_log = agent._session_log
        self._active_runner = None

    async def _runner(self, goal: str, *, max_turns: int | None = None):
        provider = await self.agent._resolve_provider()
        return self.agent._create_runner(
            self._session_log,
            goal=goal,
            session_id=self.id,
            max_turns=max_turns,
            provider=provider,
        )

    async def stream(self, goal: str, *, max_turns: int | None = None,
                     attachments: list[dict[str, Any]] | None = None):
        runner = await self._runner(goal, max_turns=max_turns)
        self._active_runner = runner
        try:
            async for event in runner.run(goal=goal, session_id=self.id, attachments=attachments):
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
            self._active_runner = await self._runner(str(started.get("goal", "")))
        try:
            async for event in self._active_runner.wake(self.id):
                yield event
        finally:
            self._active_runner = None

    def interrupt(self, reason: str = "user") -> None:
        if self._active_runner is not None:
            self._active_runner.interrupt(reason)

    async def history(self, *, from_seq: int = 0):
        """Read the durable business projection for this session."""
        return await self._session_log.read(self.id, from_seq=from_seq)

    async def latest_seq(self) -> int:
        return await self._session_log.latest_seq(self.id)

    async def replay_fixture(self):
        """Return ordered assistant messages suitable for ``ReplayProvider``."""
        from deepstrike.runtime.replay_fixture import extract_recorded_messages
        return extract_recorded_messages(await self.history())

    async def workflow_trace(self):
        """Return durable workflow lifecycle events for replay/inspection tooling."""
        workflow_kinds = {
            "workflow_batch_spawned",
            "workflow_node_completed",
            "workflow_nodes_submitted",
            "workflow_completed",
        }
        return [
            entry for entry in await self.history()
            if entry.event.get("kind") in workflow_kinds
        ]

    async def workflow_replay(self):
        """Project durable workflow events into a replayable audit object."""
        from deepstrike.runtime.workflow_replay import WorkflowReplay
        return WorkflowReplay.from_entries(await self.history())

    async def run(self, goal: str, *, max_turns: int | None = None,
                  attachments: list[dict[str, Any]] | None = None) -> str:
        from deepstrike.runtime.runner import collect_text
        return await collect_text(self.stream(goal, max_turns=max_turns, attachments=attachments))

    async def workflow(self, spec: Any):
        """Run a WorkflowSpec under this session's Kernel owner."""
        runner = await self._runner("workflow")
        self._active_runner = runner
        try:
            return await runner.run_workflow(spec, session_id=self.id)
        finally:
            self._active_runner = None

    async def remember(self, input: Mapping[str, Any] | Any):
        """Persist one host supplied memory record through the Kernel write funnel."""
        from deepstrike.memory import MemoryProvenance, MemoryRecord
        binding = self.agent.runtime_binding or {}
        store = binding.get("memory_store")
        scope = binding.get("memory_scope")
        if store is None or scope is None:
            raise RuntimeError(
                f'agent "{self.agent.name}" memory is not runtime-bound; '
                "provide runtime_binding.memory_store and memory_scope"
            )
        value = dict(input) if isinstance(input, Mapping) else vars(input)
        now = int(time.time() * 1000)
        record = MemoryRecord(
            record_id=str(value.get("record_id") or uuid.uuid4()),
            scope=scope,
            name=str(value["name"]),
            kind=value.get("kind", "reference"),
            content=str(value["content"]),
            description=str(value.get("description") or value["name"]),
            provenance=MemoryProvenance(
                author="host", trust="user_asserted", session_id=f"memory-{uuid.uuid4()}"
            ),
            created_at=int(value.get("created_at", now)),
            updated_at=int(value.get("updated_at", now)),
            confidence=float(value.get("confidence", 1.0)),
            pinned=bool(value.get("pinned", False)),
            ttl_days=value.get("ttl_days"),
        )
        runner = await self._runner("memory", max_turns=1)
        await runner.write_memory(record, session_id=record.provenance.session_id, agent_id=self.agent.name)
        return record

    async def recall(self, query: str, *, top_k: int = 8, kinds: list[str] | None = None,
                     min_score: float | None = None):
        """Query bound durable memory through the Runner memory lifecycle."""
        from deepstrike.memory import MemoryQuery

        binding = self.agent.runtime_binding or {}
        store = binding.get("memory_store")
        scope = binding.get("memory_scope")
        if store is None or scope is None:
            raise RuntimeError(
                f'agent "{self.agent.name}" memory is not runtime-bound; '
                "provide runtime_binding.memory_store and memory_scope"
            )
        runner = await self._runner("memory", max_turns=1)
        return await runner.query_memory(
            MemoryQuery(scope=scope, query=query, top_k=top_k, kinds=kinds or [], min_score=min_score),
            session_id=f"memory-{uuid.uuid4()}",
            agent_id=self.agent.name,
        )

    async def listen(self, *, lease_ms: int | None = None):
        """Claim one host signal, run it, and acknowledge only after success."""
        binding = self.agent.runtime_binding or {}
        source = binding.get("signal_source")
        if source is None:
            options = binding.get("runtime_options", {})
            source = options.get("signal_source")
        if source is None:
            raise RuntimeError("agent signals require runtime_binding.signal_source")
        claim = await source.claim_signal(self.agent.name, lease_ms)
        if claim is None:
            return None
        payload = claim.signal.payload
        goal = payload.get("goal") or payload.get("summary") or str(payload)
        try:
            result = await self.run(str(goal))
            await source.ack_signal(claim)
            return result
        except Exception:
            await source.nack_signal(claim)
            raise

    async def delegate(self, target: str, goal: str, *, metadata: Mapping[str, Any] | None = None):
        """Resolve and execute an explicitly declared handoff target."""
        from deepstrike.runtime.agent_declaration import resolve_handoff

        resolver = (self.agent.runtime_binding or {}).get("agent_resolver")
        if resolver is None:
            raise RuntimeError(f'agent "{self.agent.name}" requires runtime_binding.agent_resolver')
        allowed = {
            str(item.get("target", item.get("agent", "")))
            for item in self.agent._captured.declaration.handoffs
            if item.get("target", item.get("agent"))
        }
        if target not in allowed:
            raise PermissionError(f'agent "{self.agent.name}" cannot hand off to "{target}"')
        if callable(resolver) and not hasattr(resolver, "resolve"):
            resolved = resolver(target)
            if hasattr(resolved, "__await__"):
                resolved = await resolved
            target_agent = resolved
        else:
            resolution = resolve_handoff(self.agent._captured.declaration, target, goal, resolver)
            target_agent = resolution.target
        if target_agent is None:
            raise RuntimeError(f'target agent "{target}" is not registered')
        if not hasattr(target_agent, "run"):
            raise TypeError("agent_resolver must return an executable Agent")
        result = await target_agent.run(goal, session_id=f"handoff-{uuid.uuid4()}")
        return {"output": result.get("output", ""), "status": result.get("status", "partial"),
                **({"metadata": dict(metadata)} if metadata else {})}


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
        fallback = self.runtime_binding.get("provider_fallbacks")
        if provider is None and fallback:
            from deepstrike.providers import FallbackProvider
            provider = FallbackProvider(tuple(fallback))
        if provider is None:
            provider_for = self.runtime_binding.get("provider_for")
            provider = provider_for(self.model) if callable(provider_for) else None
        if provider is None and not (
            self.runtime_binding.get("memory_store") is not None
            and self.runtime_binding.get("memory_scope") is not None
        ):
            raise RuntimeError(f'agent "{self.name}" has no runtime provider binding')
        return provider

    def _create_runner(
        self,
        session_log: Any,
        *,
        goal: str,
        session_id: str,
        max_turns: int | None = None,
        provider: Any | None = None,
    ):
        from deepstrike.runtime.execution_plane import LocalExecutionPlane
        from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner

        binding = self.runtime_binding or {}
        options = dict(binding.get("runtime_options", {}))
        plane = options.pop("execution_plane", None)
        options.pop("session_log", None)
        for key in ("memory_store", "memory_scope", "signal_source", "knowledge_source"):
            if key in binding and key not in options:
                options[key] = binding[key]
        if max_turns is not None:
            options["max_turns"] = max_turns
        options.setdefault("run_spec", self._captured.declaration.to_run_spec(goal=goal, session_id=session_id))
        if plane is None:
            plane = LocalExecutionPlane()
            if self._captured.host_tools:
                plane.register(*self._captured.host_tools)
        return RuntimeRunner(RuntimeOptions(
            provider=provider or self._provider(),
            execution_plane=plane,
            session_log=session_log,
            agent_id=self.name,
            **({"system_prompt": self.instructions} if self.instructions else {}),
            **options,
        ))

    async def _resolve_provider(self) -> Any:
        binding = self.runtime_binding or {}
        if binding.get("provider") is not None:
            return binding["provider"]
        fallback = binding.get("provider_fallbacks")
        if fallback:
            from deepstrike.providers import FallbackProvider
            return FallbackProvider(tuple(fallback))
        if binding.get("memory_store") is not None and binding.get("memory_scope") is not None:
            return _MemoryOnlyProvider()
        provider_for = binding.get("provider_for")
        if callable(provider_for):
            resolved = provider_for(self.model)
            if resolved is not None:
                return resolved
        candidates = binding.get("provider_candidates")
        if candidates:
            from deepstrike.providers import CapabilityRequirement, CapabilityRouter

            requirement_data = self.model if isinstance(self.model, Mapping) else {}
            requirement = CapabilityRequirement(
                tools=requirement_data.get("tools"),
                reasoning=requirement_data.get("reasoning"),
                minimum_context_window=requirement_data.get("context_window"),
            )
            result = await CapabilityRouter().route(requirement, candidates)
            if result.ok and result.provider is not None:
                return result.provider
            raise RuntimeError(result.error or {"code": "no_capable_model"})
        raise RuntimeError(f'agent "{self.name}" has no runtime provider binding')

    async def workflow(self, spec: Any, *, session_id: str | None = None):
        """Run a declarative workflow through the shared Kernel path."""
        return await self.session(session_id).workflow(spec)

    def save_workflow(self, name: str, spec: Any) -> str:
        store = (self.runtime_binding or {}).get("workflow_store")
        if store is None:
            raise RuntimeError("workflow persistence requires runtime_binding.workflow_store")
        return store.save(name, spec)

    def load_workflow(self, name: str) -> Any:
        store = (self.runtime_binding or {}).get("workflow_store")
        if store is None:
            raise RuntimeError("workflow persistence requires runtime_binding.workflow_store")
        return store.load(name)

    def list_workflows(self) -> list[str]:
        store = (self.runtime_binding or {}).get("workflow_store")
        if store is None:
            raise RuntimeError("workflow persistence requires runtime_binding.workflow_store")
        return store.list()

    async def run(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None,
                  attachments: list[dict[str, Any]] | None = None) -> dict[str, Any]:
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
        output = await self.session(resolved_session_id).run(goal, max_turns=max_turns, attachments=attachments)
        entries = await self.session(resolved_session_id).history()
        events = [entry.event for entry in entries]
        started = next((event for event in reversed(events) if event.get("kind") == "run_started"), {})
        terminal = next((event for event in reversed(events) if event.get("kind") == "run_terminal"), {})
        reason = str(terminal.get("reason", "completed"))
        status = "completed" if reason in {"completed", "success", "done"} else (
            "cancelled" if reason in {"user", "user_abort", "deadline", "lease_lost", "host_shutdown", "timeout"}
            else "failed" if reason in {"error", "failed", "invalid_arg"} else "partial"
        )
        result: dict[str, Any] = {
            "output": output,
            "status": status,
            "session_id": resolved_session_id,
            "run_id": started.get("run_id"),
        }
        if self.output_schema:
            from deepstrike.runtime.output_schema import extract_json_value, validate_against_schema
            errors = validate_against_schema(extract_json_value(output), self.output_schema)
            result["output_validation"] = {"ok": not errors, "errors": errors}
            if errors and status == "completed":
                result["status"] = "failed"
        attempts = next((event for event in reversed(events) if event.get("kind") == "provider_attempt"), {})
        measured = next((event for event in reversed(events) if event.get("kind") == "prompt_measured"), {})
        prepared = next((event for event in reversed(events) if event.get("kind") == "context_prepared"), {})
        evidence = {
            **({"route": attempts.get("route")} if attempts.get("route") is not None else {}),
            **({"measurement": measured.get("measurement")} if measured.get("measurement") is not None else {}),
            **({"context_binding": prepared.get("preparation", {}).get("binding")} if isinstance(prepared.get("preparation"), dict) and prepared["preparation"].get("binding") is not None else {}),
        }
        if evidence:
            result["evidence"] = evidence
        if terminal.get("total_tokens") is not None:
            result["usage"] = {"total_tokens": terminal["total_tokens"]}
        return result

    async def stream(self, goal: str, *, session_id: str | None = None, max_turns: int | None = None,
                     attachments: list[dict[str, Any]] | None = None):
        """Stream host events for the same public Agent contract."""
        resolved_session_id = session_id or f"agent-{uuid.uuid4()}"
        async for event in self.session(resolved_session_id).stream(goal, max_turns=max_turns, attachments=attachments):
            yield event

    async def remember(self, input: Mapping[str, Any] | Any, *, session_id: str | None = None):
        return await self.session(session_id).remember(input)

    async def recall(self, query: str, *, top_k: int = 8, kinds: list[str] | None = None,
                     min_score: float | None = None, session_id: str | None = None):
        return await self.session(session_id).recall(query, top_k=top_k, kinds=kinds, min_score=min_score)

    async def delegate(self, target: str, goal: str, *, metadata: Mapping[str, Any] | None = None):
        return await self.session().delegate(target, goal, metadata=metadata)

    async def listen(self, *, lease_ms: int | None = None, session_id: str | None = None):
        return await self.session(session_id).listen(lease_ms=lease_ms)

    def interrupt(self, reason: str = "user", *, session_id: str | None = None) -> None:
        if session_id is not None:
            self.session(session_id).interrupt(reason)
            return
        for session in self._sessions.values():
            session.interrupt(reason)

    async def close(self) -> None:
        plane = (self.runtime_binding or {}).get("mcp_plane")
        if plane is not None and hasattr(plane, "disconnect"):
            await plane.disconnect()


def create_agent(name: str, **kwargs: Any) -> Agent:
    """Create the executable public Agent handle."""
    return Agent(name, **kwargs)
