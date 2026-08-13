"""Canonical, provider-neutral lowering for public :class:`deepstrike.Agent` declarations."""
from __future__ import annotations

from copy import deepcopy
import json
from typing import Any, Mapping

from deepstrike.agent import Agent, AgentDefinition, MemoryReference
from deepstrike.memory import DurableMemory, WorkingMemory
from deepstrike.tools import RegisteredTool
from deepstrike.types.agent import AgentCapabilityFilter


def _mapping(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise TypeError(f"agent {field} must be an object")
    return dict(value)


def _optional_mapping(raw: Mapping[str, Any], camel: str, snake: str) -> Mapping[str, Any] | None:
    value = raw.get(camel, raw.get(snake))
    return _mapping(value, camel) if value is not None else None


def normalize_agent(agent: Agent | AgentDefinition) -> Agent:
    """Accept a native Agent or JSON-safe descriptor without interpreting provider namespaces."""
    if isinstance(agent, Agent):
        return agent
    raw = _mapping(agent, "definition")
    name = raw.get("name")
    if not isinstance(name, str) or not name:
        raise ValueError("agent name is required")
    sequence_fields = {
        "tools": "tools",
        "mcpServers": "mcp_servers",
        "skills": "skills",
        "knowledge": "knowledge",
        "handoffs": "handoffs",
        "guardrails": "guardrails",
    }
    sequences: dict[str, list[Any] | None] = {}
    for camel, snake in sequence_fields.items():
        value = raw.get(camel, raw.get(snake))
        if value is not None and (not isinstance(value, list) or any(not isinstance(item, Mapping) for item in value)):
            raise TypeError(f"agent {camel} must be an array of objects")
        sequences[snake] = [deepcopy(dict(item)) for item in value] if value is not None else None
    memory = raw.get("memory")
    if memory is not None and isinstance(memory, Mapping):
        memory = deepcopy(dict(memory))
    return Agent(
        name,
        description=raw.get("description"),
        instructions=raw.get("instructions"),
        model=deepcopy(raw["model"]) if raw.get("model") is not None else None,
        capability_filter=_optional_mapping(raw, "capabilityFilter", "capability_filter"),
        tools=sequences["tools"],
        mcp_servers=sequences["mcp_servers"],
        skills=sequences["skills"],
        memory=memory,
        knowledge=sequences["knowledge"],
        handoffs=sequences["handoffs"],
        provider_options=_optional_mapping(raw, "providerOptions", "provider_options"),
        output_schema=_optional_mapping(raw, "outputSchema", "output_schema"),
        metadata=_optional_mapping(raw, "metadata", "metadata"),
        guardrails=sequences["guardrails"],
    )


def _lower_tool(tool: Any) -> dict[str, Any]:
    if isinstance(tool, Mapping):
        raw = _mapping(tool, "tool")
        name = raw.get("name")
        if not isinstance(name, str) or not name:
            raise ValueError("tool name is required")
        parameters = raw.get("parameters", {"type": "object", "properties": {}})
        if not isinstance(parameters, Mapping) or parameters.get("type") != "object":
            raise ValueError(f'tool "{name}": parameters must be a JSON Schema with root type "object"')
        return {
            "name": name,
            "description": str(raw.get("description", "")),
            "parameters": deepcopy(dict(parameters)),
            **({"providerOptions": deepcopy(dict(raw["providerOptions"]))} if isinstance(raw.get("providerOptions"), Mapping) else {}),
        }
    if not isinstance(tool, RegisteredTool):
        raise TypeError("tools must be RegisteredTool instances or declarative tool objects")
    schema = tool.schema
    try:
        parameters = json.loads(schema.parameters)
    except (TypeError, ValueError) as exc:
        raise ValueError(f'tool "{schema.name}" has invalid JSON Schema parameters') from exc
    if not isinstance(parameters, dict) or parameters.get("type") != "object":
        raise ValueError(f'tool "{schema.name}": parameters must be a JSON Schema with root type "object"')
    provider_options = getattr(tool, "provider_options", None)
    return {
        "name": schema.name,
        "description": schema.description,
        "parameters": deepcopy(parameters),
        **({"providerOptions": deepcopy(dict(provider_options))} if isinstance(provider_options, Mapping) else {}),
    }


def _lower_memory(memory: Any) -> dict[str, Any] | None:
    if memory is None:
        return None
    if isinstance(memory, WorkingMemory):
        return {"kind": "working"}
    if isinstance(memory, DurableMemory):
        return {"kind": "durable", **({"namespace": memory.namespace} if memory.namespace else {})}
    if isinstance(memory, MemoryReference):
        return {"kind": "durable", **({"namespace": memory.namespace} if memory.namespace else {})}
    if isinstance(memory, Mapping):
        raw = _mapping(memory, "memory")
        kind = raw.get("kind", "durable")
        if kind != "durable":
            raise ValueError('declarative memory kind must be "durable"')
        namespace = raw.get("namespace")
        if namespace is not None and not isinstance(namespace, str):
            raise TypeError("memory namespace must be a string")
        return {"kind": "durable", **({"namespace": namespace} if namespace else {})}
    raise TypeError("memory must be WorkingMemory, DurableMemory, MemoryReference, or a declarative object")


def _lower_filter(filter_value: AgentCapabilityFilter | Mapping[str, Any] | None) -> dict[str, list[str]] | None:
    if filter_value is None:
        return None
    if isinstance(filter_value, AgentCapabilityFilter):
        allowed_kinds = filter_value.allowed_kinds
        allowed_ids = filter_value.allowed_ids
    elif isinstance(filter_value, Mapping):
        allowed_kinds = filter_value.get("allowedKinds", filter_value.get("allowed_kinds", []))
        allowed_ids = filter_value.get("allowedIds", filter_value.get("allowed_ids", []))
    else:
        raise TypeError("capability_filter must be AgentCapabilityFilter or an object")
    if not isinstance(allowed_kinds, list) or not all(isinstance(value, str) for value in allowed_kinds):
        raise TypeError("capability_filter allowedKinds must be an array of strings")
    if not isinstance(allowed_ids, list) or not all(isinstance(value, str) for value in allowed_ids):
        raise TypeError("capability_filter allowedIds must be an array of strings")
    return {"allowedKinds": list(allowed_kinds), "allowedIds": list(allowed_ids)}


def _capability_allowed(capability: Mapping[str, str], filter_value: Mapping[str, list[str]] | None) -> bool:
    if filter_value is None:
        return True
    kinds = filter_value["allowedKinds"]
    ids = filter_value["allowedIds"]
    return (not kinds or capability["kind"] in kinds) and (not ids or capability["id"] in ids)


def _clone_declarations(values: list[Mapping[str, Any]] | None, field: str) -> list[dict[str, Any]]:
    if not values:
        return []
    return [deepcopy(_mapping(value, field)) for value in values]


def lower_agent(agent: Agent) -> dict[str, Any]:
    """Produce a detached canonical descriptor without provider routing or capability grants."""
    if not isinstance(agent, Agent):
        raise TypeError("lower_agent expects an Agent; use normalize_agent for JSON-safe definitions")
    tools = [_lower_tool(tool) for tool in agent.tools or []]
    mcp_servers = _clone_declarations(agent.mcp_servers, "mcpServers")
    skills = _clone_declarations(agent.skills, "skills")
    knowledge = _clone_declarations(agent.knowledge, "knowledge")
    handoffs = _clone_declarations(agent.handoffs, "handoffs")
    guardrails = _clone_declarations(agent.guardrails, "guardrails")
    memory = _lower_memory(agent.memory)
    capability_filter = _lower_filter(agent.capability_filter)
    capabilities = [
        *({"kind": "tool", "id": tool["name"], "description": tool["description"]} for tool in tools),
        *({
            "kind": "mcp_server",
            "id": str(server.get("name") or _mapping(server.get("transport", {}), "mcp transport").get("kind", "")),
            "description": str(server.get("name") or f'{_mapping(server.get("transport", {}), "mcp transport").get("kind", "")} MCP server'),
        } for server in mcp_servers),
        *({"kind": "skill", "id": str(skill.get("name", "")), "description": str(skill.get("description", ""))} for skill in skills),
    ]
    effective = [capability for capability in capabilities if _capability_allowed(capability, capability_filter)]
    extensions = deepcopy(agent.provider_options or {})
    spec: dict[str, Any] = {
        "name": agent.name,
        **({"description": agent.description} if agent.description else {}),
        **({"instructions": agent.instructions} if agent.instructions else {}),
        **({"model": deepcopy(agent.model)} if agent.model else {}),
        "tools": tools,
        **({"outputSchema": deepcopy(agent.output_schema)} if agent.output_schema else {}),
        **({"mcpServers": mcp_servers} if mcp_servers else {}),
        **({"skills": skills} if skills else {}),
        **({"memory": memory} if memory else {}),
        **({"knowledge": knowledge} if knowledge else {}),
        **({"handoffs": handoffs} if handoffs else {}),
        **({"guardrails": guardrails} if guardrails else {}),
        **({"metadata": deepcopy(agent.metadata)} if agent.metadata else {}),
        "capabilities": capabilities,
        **({"capabilityFilter": capability_filter} if capability_filter is not None else {}),
        "effectiveCapabilities": effective,
        "extensions": extensions,
        "providerOptions": deepcopy(extensions),
        "inputs": {
            "run": {"name": agent.name, **({"model": deepcopy(agent.model)} if agent.model else {})},
            "context": {
                **({"description": agent.description} if agent.description else {}),
                **({"instructions": agent.instructions} if agent.instructions else {}),
                **({"outputSchema": deepcopy(agent.output_schema)} if agent.output_schema else {}),
                "knowledge": knowledge,
            },
            "capabilities": {"tools": tools, "mcpServers": mcp_servers, "skills": skills, "effective": effective},
            **({"memory": memory} if memory else {}),
            "delegation": {"handoffs": handoffs},
            "governance": {"guardrails": guardrails},
        },
    }
    return spec
