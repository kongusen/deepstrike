from __future__ import annotations
import json
from deepstrike._kernel import ToolCall, ToolResult
from collections.abc import AsyncIterable
from .errors import format_tool_error
from .registry import RegisteredTool, tool_chunk_text, validate_tool_arguments


async def execute_tools(
    calls: list[ToolCall],
    registry: dict[str, RegisteredTool],
) -> list[ToolResult]:
    results = []
    for call in calls:
        tool = registry.get(call.name)
        if tool is None:
            results.append(ToolResult(call_id=call.id, output=f"unknown tool: {call.name}", is_error=True))
            continue
        try:
            kwargs = json.loads(call.arguments)
            validation = validate_tool_arguments(tool.schema.parameters, kwargs)
            if validation.get("error"):
                results.append(ToolResult(call_id=call.id, output=f"invalid arguments: {validation['error']}", is_error=True))
                continue
            # validation["args"], not kwargs: a oneOf/anyOf ROOT accepts a repaired probe
            # deep-copy — the original dict never sees those repairs (auto-casts, strips, defaults).
            output = await tool(**validation["args"])
            if isinstance(output, AsyncIterable):
                chunks = []
                async for chunk in output:
                    chunks.append(tool_chunk_text(chunk))
                output = "".join(chunks)
            results.append(ToolResult(call_id=call.id, output=str(output)))
        except Exception as exc:
            results.append(ToolResult(call_id=call.id, output=format_tool_error(exc), is_error=True))
    return results
