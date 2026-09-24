"""Python Agent declaration capture and lowering boundary.

The declaration is the serializable input to the shared Canonical Kernel. Python
callables and provider objects stay in host bindings and never cross this boundary.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Protocol, Sequence, runtime_checkable


@dataclass(frozen=True)
class AgentDeclaration:
    name: str
    description: str | None = None
    instructions: str | None = None
    model: str | dict[str, Any] | None = None
    capability_filter: Any = None
    tools: tuple[dict[str, Any], ...] = ()
    mcp_servers: tuple[dict[str, Any], ...] = ()
    skills: tuple[dict[str, Any], ...] = ()
    memory: Any = None
    knowledge: tuple[dict[str, Any], ...] = ()
    handoffs: tuple[dict[str, Any], ...] = ()
    provider_options: dict[str, Any] | None = None
    output_schema: dict[str, Any] | None = None
    metadata: dict[str, Any] | None = None
    guardrails: tuple[dict[str, Any], ...] = ()

    def to_kernel_dict(self) -> dict[str, Any]:
        """Return only JSON-safe declaration data for runtime/kernel adapters."""
        return {
            "name": self.name,
            **({"description": self.description} if self.description is not None else {}),
            **({"instructions": self.instructions} if self.instructions is not None else {}),
            **({"model": self.model} if self.model is not None else {}),
            **({"capability_filter": self.capability_filter} if self.capability_filter is not None else {}),
            "tools": [dict(tool) for tool in self.tools],
            "mcp_servers": [dict(server) for server in self.mcp_servers],
            "skills": [dict(skill) for skill in self.skills],
            **({"memory": self.memory} if self.memory is not None else {}),
            "knowledge": [dict(item) for item in self.knowledge],
            "handoffs": [dict(item) for item in self.handoffs],
            **({"provider_options": dict(self.provider_options)} if self.provider_options is not None else {}),
            **({"output_schema": dict(self.output_schema)} if self.output_schema is not None else {}),
            **({"metadata": dict(self.metadata)} if self.metadata is not None else {}),
            "guardrails": [dict(item) for item in self.guardrails],
        }

    def to_run_spec(self, *, goal: str, session_id: str, role: str = "custom") -> Any:
        """Lower the declaration to the shared Python ``AgentRunSpec`` contract."""
        from deepstrike.types.agent import AgentCapabilityFilter, AgentIdentity, AgentRunSpec

        capability_filter = self.capability_filter
        if isinstance(capability_filter, Mapping):
            capability_filter = AgentCapabilityFilter(
                allowed_kinds=list(capability_filter.get("allowed_kinds", capability_filter.get("allowedKinds", [])) or []),
                allowed_ids=list(capability_filter.get("allowed_ids", capability_filter.get("allowedIds", [])) or []),
            )
        return AgentRunSpec(
            identity=AgentIdentity(agent_id=self.name, session_id=session_id),
            role=role,
            goal=goal,
            capability_filter=capability_filter,
            metadata=dict(self.metadata) if self.metadata is not None else None,
            model_hint=self.model if isinstance(self.model, str) else None,
            exposure_baseline=[tool["name"] for tool in self.tools if tool.get("name")],
        )


@dataclass(frozen=True)
class CapturedAgent:
    declaration: AgentDeclaration
    host_tools: tuple[Any, ...] = ()
    runtime_binding: Mapping[str, Any] | None = None


@runtime_checkable
class AgentResolver(Protocol):
    """Host-side name resolver; resolved declarations still lower through the Kernel contract."""

    def resolve(self, name: str) -> CapturedAgent: ...


class InMemoryAgentResolver:
    def __init__(self, agents: Sequence[CapturedAgent] | None = None) -> None:
        self._agents: dict[str, CapturedAgent] = {}
        for agent in agents or ():
            self.register(agent)

    def register(self, agent: CapturedAgent) -> "InMemoryAgentResolver":
        name = agent.declaration.name
        if name in self._agents and self._agents[name] is not agent:
            raise ValueError(f'agent "{name}" is already registered')
        self._agents[name] = agent
        return self

    def resolve(self, name: str) -> CapturedAgent:
        try:
            return self._agents[name]
        except KeyError as exc:
            raise KeyError(f'unknown agent "{name}"') from exc


def _tool_snapshot(tool: Any) -> tuple[dict[str, Any], Any | None]:
    schema = getattr(tool, "schema", None)
    if schema is not None:
        parameters = getattr(schema, "parameters", {})
        return {
            "name": str(getattr(schema, "name", "")),
            "description": str(getattr(schema, "description", "")),
            "parameters": parameters,
        }, tool
    if isinstance(tool, Mapping):
        if not tool.get("name"):
            raise ValueError("tool declaration requires a name")
        return dict(tool), None
    raise TypeError("tools must be RegisteredTool instances or mappings")


def capture_agent_declaration(
    name: str,
    *,
    description: str | None = None,
    instructions: str | None = None,
    model: str | dict[str, Any] | None = None,
    capability_filter: Any = None,
    tools: Sequence[Any] | None = None,
    mcp_servers: Sequence[Mapping[str, Any]] | None = None,
    skills: Sequence[Mapping[str, Any]] | None = None,
    memory: Any = None,
    knowledge: Sequence[Mapping[str, Any]] | None = None,
    handoffs: Sequence[Mapping[str, Any]] | None = None,
    provider_options: Mapping[str, Any] | None = None,
    output_schema: Mapping[str, Any] | None = None,
    metadata: Mapping[str, Any] | None = None,
    guardrails: Sequence[Mapping[str, Any]] | None = None,
    runtime_binding: Mapping[str, Any] | None = None,
) -> CapturedAgent:
    if not name:
        raise ValueError("agent name is required")
    binding = runtime_binding or {}
    if (binding.get("memory_store") is None) != (binding.get("memory_scope") is None):
        raise ValueError("runtime_binding.memory_store and memory_scope must be configured together")

    snapshots: list[dict[str, Any]] = []
    host_tools: list[Any] = []
    for tool in tools or ():
        snapshot, host_tool = _tool_snapshot(tool)
        snapshots.append(snapshot)
        if host_tool is not None:
            host_tools.append(host_tool)

    def mappings(values: Sequence[Mapping[str, Any]] | None, label: str) -> tuple[dict[str, Any], ...]:
        result: list[dict[str, Any]] = []
        for index, value in enumerate(values or ()):
            if not isinstance(value, Mapping):
                raise TypeError(f"{label}[{index}] must be a mapping")
            result.append(dict(value))
        return tuple(result)

    declaration = AgentDeclaration(
        name=name,
        description=description,
        instructions=instructions,
        model=model,
        capability_filter=capability_filter,
        tools=tuple(snapshots),
        mcp_servers=mappings(mcp_servers, "mcp_servers"),
        skills=mappings(skills, "skills"),
        memory=memory,
        knowledge=mappings(knowledge, "knowledge"),
        handoffs=mappings(handoffs, "handoffs"),
        provider_options=dict(provider_options) if provider_options is not None else None,
        output_schema=dict(output_schema) if output_schema is not None else None,
        metadata=dict(metadata) if metadata is not None else None,
        guardrails=mappings(guardrails, "guardrails"),
    )
    return CapturedAgent(declaration, tuple(host_tools), runtime_binding)
