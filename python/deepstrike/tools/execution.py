from __future__ import annotations
import json
from deepstrike._kernel import ToolCall, ToolResult
from .registry import RegisteredTool


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
            output = await tool(**kwargs)
            results.append(ToolResult(call_id=call.id, output=output))
        except Exception as exc:
            results.append(ToolResult(call_id=call.id, output=str(exc), is_error=True))
    return results
