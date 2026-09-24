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
        output_validation: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(
            output=output,
            run_id=run_id,
            session_id=session_id,
            status=status,
            usage=usage,
            evidence=evidence or [],
            output_validation=output_validation,
        )

    output: str
    run_id: str | None
    session_id: str
    status: str
    usage: dict[str, Any] | None
    evidence: list[dict[str, Any]]
    output_validation: dict[str, Any] | None

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
        criteria: list[str] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        extensions: dict[str, Any] | None = None,
    ) -> AsyncIterator[Any]:
        runner = await self.agent._prepare_runner(self.id, max_turns=max_turns)
        async for event in runner.run(
            goal=goal,
            session_id=self.id,
            criteria=criteria,
            attachments=attachments,
            extensions=extensions,
        ):
            yield event

    async def resume(self) -> AsyncIterator[Any]:
        runner = await self.agent._prepare_runner(self.id)
        async for event in runner.wake(self.id):
            yield event

    def interrupt(self, reason: str = "user") -> None:
        self._runner.interrupt(reason)  # type: ignore[arg-type]

    async def run(
        self,
        goal: str,
        *,
        max_turns: int | None = None,
        criteria: list[str] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        extensions: dict[str, Any] | None = None,
    ) -> RunResult:
        return await self.agent._run_in_session(
            self,
            goal,
            max_turns=max_turns,
            criteria=criteria,
            attachments=attachments,
            extensions=extensions,
        )


class AgentRegistry:
    """Name based Agent resolver for workflow and delegation hosts."""

    def __init__(self, agents: Sequence["Agent"] | None = None) -> None:
        self._agents: dict[str, Agent] = {}
        for agent in agents or []:
            self.register(agent)

    def register(self, agent: "Agent") -> "AgentRegistry":
        if not isinstance(agent, Agent):
            raise TypeError("agent registry accepts Agent instances")
        if agent.name in self._agents and self._agents[agent.name] is not agent:
            raise ValueError(f'agent "{agent.name}" is already registered')
        self._agents[agent.name] = agent
        return self

    def resolve(self, name: str) -> "Agent":
        try:
            return self._agents[name]
        except KeyError as exc:
            raise KeyError(f'unknown agent "{name}"') from exc

    def get(self, name: str) -> "Agent | None":
        return self._agents.get(name)

    def names(self) -> tuple[str, ...]:
        return tuple(self._agents)


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
        self._validate_declaration()
        self._session_log = None
        self._runners: dict[str, Any] = {}
        self._mcp_planes: list[Any] = []
        self._managed_planes: list[Any] = []
        self._connected_planes: set[int] = set()

    def _validate_declaration(self) -> None:
        """Validate host bindings once, before a run can create kernel state."""
        binding = self.runtime_binding or {}
        has_store = binding.get("memory_store") is not None
        has_scope = binding.get("memory_scope") is not None
        if has_store != has_scope:
            raise ValueError("runtime_binding.memory_store and memory_scope must be configured together")
        for index, server in enumerate(self.mcp_servers or []):
            if not isinstance(server, Mapping):
                raise TypeError(f"mcp_servers[{index}] must be a mapping")
            if not server.get("name"):
                raise ValueError(f"mcp_servers[{index}] requires a name")
            if not server.get("command") and binding.get("mcp_execution_plane") is None:
                raise ValueError(f"mcp_servers[{index}] requires command when no bound MCP plane is provided")
        for index, skill in enumerate(self.skills or []):
            if not isinstance(skill, Mapping) or not skill.get("name"):
                raise ValueError(f"skills[{index}] requires a name")
        if self.skills and not binding.get("skill_dir"):
            raise ValueError("skills require runtime_binding.skill_dir")
        if self.output_schema is not None and not isinstance(self.output_schema, Mapping):
            raise TypeError("output_schema must be a mapping")

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
        if self.mcp_servers and binding.get("mcp_execution_plane") is None and binding.get("execution_plane") is None:
            raise RuntimeError(
                f'agent "{self.name}" declares mcp_servers but no mcp execution plane is bound'
            )
        if self.provider_options:
            raw_options["extensions"] = {
                **dict(raw_options.get("extensions") or {}),
                **self.provider_options,
            }
        for option_name in ("skill_dir", "skill_filter", "knowledge_source", "governance_policy"):
            if binding.get(option_name) is not None:
                raw_options.setdefault(option_name, binding[option_name])
        if self.skills:
            declared_names = [str(skill["name"]) for skill in self.skills]
            raw_options["skill_filter"] = list(raw_options.get("skill_filter") or declared_names)
        if binding.get("memory_store") is not None:
            raw_options.setdefault("memory_store", binding["memory_store"])
        if binding.get("memory_scope") is not None:
            raw_options.setdefault("memory_scope", binding["memory_scope"])
        configured_plane = (self.runtime_binding or {}).get("execution_plane") or (self.runtime_binding or {}).get("mcp_execution_plane")
        if configured_plane is None and self.mcp_servers:
            from deepstrike.runtime.credential_vault import EnvCredentialVault
            from deepstrike.runtime.mcp_proxy_plane import McpProxyPlane, McpServerConfig

            servers = {
                str(server["name"]): McpServerConfig(
                    command=str(server["command"]),
                    args=list(server.get("args") or []),
                    credential_keys=list(server.get("credential_keys") or server.get("credentialKeys") or []),
                    env=dict(server.get("env") or {}),
                )
                for server in self.mcp_servers
            }
            configured_plane = McpProxyPlane(
                servers=servers,
                vault=(self.runtime_binding or {}).get("credential_vault") or EnvCredentialVault(),
            )
            self._mcp_planes.append(configured_plane)
        plane = configured_plane or LocalExecutionPlane()
        if hasattr(plane, "disconnect") and all(existing is not plane for existing in self._managed_planes):
            self._managed_planes.append(plane)
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

    async def _prepare_runner(self, session_id: str, *, max_turns: int | None = None) -> Any:
        runner = self._runner_for(session_id, max_turns=max_turns)
        plane = runner.execution_plane
        if hasattr(plane, "connect") and id(plane) not in self._connected_planes:
            await plane.connect()
            self._connected_planes.add(id(plane))
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

    async def delegate(
        self,
        goal: str,
        *,
        role: str = "custom",
        session_id: str | None = None,
    ) -> Any:
        """Delegate one bounded task through the governed workflow scheduler."""
        from deepstrike.types.agent import WorkflowNodeSpec, WorkflowSpec

        spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task=goal, role=role)])
        return await self.workflow(spec, session_id=session_id)

    async def _collect_result(self, session: AgentSession, events: AsyncIterator[Any]) -> RunResult:
        from deepstrike.providers.stream import DoneEvent, ErrorEvent, TextDelta, UsageEvent

        output: list[str] = []
        usage: dict[str, Any] | None = None
        status = "completed"
        async for event in events:
            event_type = event.get("type") if isinstance(event, dict) else getattr(event, "type", None)
            if isinstance(event, TextDelta) or event_type == "text_delta":
                output.append(event.get("delta", "") if isinstance(event, dict) else event.delta)
            elif isinstance(event, UsageEvent) or event_type == "usage":
                usage = {
                    "input_tokens": event.get("input_tokens", event.get("inputTokens", 0)) if isinstance(event, dict) else event.input_tokens,
                    "output_tokens": event.get("output_tokens", event.get("outputTokens", 0)) if isinstance(event, dict) else event.output_tokens,
                    "total_tokens": event.get("total_tokens", event.get("totalTokens", 0)) if isinstance(event, dict) else event.total_tokens,
                }
            elif isinstance(event, DoneEvent) or event_type == "done":
                status = event.get("status", "completed") if isinstance(event, dict) else event.status
            elif isinstance(event, ErrorEvent) or event_type == "error":
                status = "error"
        entries = await self._session_log.read(session.id) if self._session_log is not None else []
        started = next((entry.event for entry in entries if entry.event.get("kind") == "run_started"), None)
        evidence_kinds = {
            "run_terminal",
            "provider_attempt",
            "llm_completed",
            "context_prepared",
            "prompt_measured",
            "tool_requested",
            "tool_completed",
            "memory_retrieval_result",
            "workflow_completed",
        }
        evidence = [entry.event for entry in entries if entry.event.get("kind") in evidence_kinds]
        if usage is None:
            attempts = [event for event in evidence if event.get("kind") == "provider_attempt"]
            usage_records = [event.get("usage") for event in attempts if isinstance(event.get("usage"), dict)]
            if usage_records:
                usage = {
                    "input_tokens": sum(int(item.get("input_tokens", item.get("inputTokens", 0)) or 0) for item in usage_records),
                    "output_tokens": sum(int(item.get("output_tokens", item.get("outputTokens", 0)) or 0) for item in usage_records),
                    "total_tokens": sum(int(item.get("total_tokens", item.get("totalTokens", 0)) or 0) for item in usage_records),
                }
        terminal = next((event for event in reversed(evidence) if event.get("kind") == "run_terminal"), None)
        if terminal is not None:
            reason = terminal.get("reason")
            if reason in {"user_abort", "timeout", "token_budget", "max_turns"}:
                status = "cancelled" if reason == "user_abort" else "partial"
            elif reason == "error":
                status = "error"
        output_validation = None
        if self.output_schema is not None:
            from deepstrike.runtime.output_schema import extract_json_value, validate_against_schema
            parsed = extract_json_value("".join(output))
            errors = validate_against_schema(parsed, self.output_schema)
            output_validation = {
                "valid": not errors,
                "value": parsed,
                "errors": errors,
            }
            if errors:
                status = "partial"
        return RunResult(
            output="".join(output),
            run_id=started.get("run_id") if started else None,
            session_id=session.id,
            status=status,
            usage=usage,
            evidence=evidence,
            output_validation=output_validation,
        )

    async def _run_in_session(
        self,
        session: AgentSession,
        goal: str,
        *,
        max_turns: int | None = None,
        criteria: list[str] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        extensions: dict[str, Any] | None = None,
    ) -> RunResult:
        return await self._collect_result(
            session,
            session.stream(
                goal,
                max_turns=max_turns,
                criteria=criteria,
                attachments=attachments,
                extensions=extensions,
            ),
        )

    async def run(
        self,
        goal: str,
        *,
        session_id: str | None = None,
        max_turns: int | None = None,
        criteria: list[str] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        extensions: dict[str, Any] | None = None,
    ) -> RunResult:
        """Execute one goal and return a reusable, structured result."""
        session = self.session(session_id)
        return await self._run_in_session(
            session,
            goal,
            max_turns=max_turns,
            criteria=criteria,
            attachments=attachments,
            extensions=extensions,
        )

    async def listen(self, session_id: str) -> RunResult | None:
        """Resume a durable session that has pending inbound signals or work."""
        session = self.session(session_id)
        try:
            return await self._collect_result(session, session.resume())
        except ValueError:
            return None

    async def evaluate(
        self,
        goal: str,
        criteria: Sequence[Any],
        *,
        result: RunResult | str | None = None,
        eval_provider: Any | None = None,
    ) -> Any:
        """Run the SDK's stateless judge against this Agent's output."""
        from deepstrike.runtime.eval import Criterion, judge

        run_result = result if isinstance(result, RunResult) else None
        output = result if isinstance(result, str) else None
        if run_result is None and output is None:
            run_result = await self.run(goal)
            output = run_result.output
        normalized = [item if isinstance(item, Criterion) else Criterion(text=str(item)) for item in criteria]
        provider = eval_provider or (self.runtime_binding or {}).get("eval_provider") or self._provider()
        verdict = await judge(provider, goal, normalized, output or "")
        return {"result": run_result, "output": output or "", "verdict": verdict}

    async def replay(
        self,
        messages: Sequence[Mapping[str, Any]],
        goal: str,
        *,
        session_id: str | None = None,
    ) -> RunResult:
        """Execute a deterministic run using recorded assistant messages."""
        from deepstrike.runtime.replay_provider import ReplayProvider

        replay_binding = dict(self.runtime_binding or {})
        replay_binding["provider"] = ReplayProvider(list(messages))
        replay_binding.pop("provider_for", None)
        replay_agent = Agent(
            self.name,
            description=self.description,
            instructions=self.instructions,
            model=self.model,
            capability_filter=self.capability_filter,
            tools=self.tools,
            mcp_servers=self.mcp_servers,
            skills=self.skills,
            memory=self.memory,
            knowledge=self.knowledge,
            handoffs=self.handoffs,
            provider_options=self.provider_options,
            output_schema=self.output_schema,
            metadata=self.metadata,
            guardrails=self.guardrails,
            runtime_binding=replay_binding,
        )
        return await replay_agent.run(goal, session_id=session_id)

    async def stream(
        self,
        goal: str,
        *,
        session_id: str | None = None,
        max_turns: int | None = None,
        criteria: list[str] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        extensions: dict[str, Any] | None = None,
    ):
        """Stream host events while retaining the session for later resume."""
        session = self.session(session_id)
        async for event in session.stream(
            goal,
            max_turns=max_turns,
            criteria=criteria,
            attachments=attachments,
            extensions=extensions,
        ):
            yield event

    def close(self) -> None:
        """Interrupt active runs owned by this agent."""
        for runner in self._runners.values():
            runner.interrupt("host_shutdown")

    async def aclose(self) -> None:
        """Interrupt active runs and close async execution resources such as MCP servers."""
        self.close()
        for plane in self._managed_planes:
            if hasattr(plane, "disconnect"):
                await plane.disconnect()
        self._connected_planes.clear()


def create_agent(name: str, **kwargs: Any) -> Agent:
    """Create the executable public Agent handle."""
    return Agent(name, **kwargs)
