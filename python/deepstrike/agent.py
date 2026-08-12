"""Public, declarative Agent descriptor.

This surface describes an agent before a host chooses a provider, execution plane, or Kernel run.
It deliberately has no run method: ``AgentRunSpec`` remains the runtime-to-Kernel contract.
"""
from __future__ import annotations

from dataclasses import dataclass
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


class Agent:
    """Provider-neutral declaration consumed by :func:`deepstrike.lower_agent`.

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
