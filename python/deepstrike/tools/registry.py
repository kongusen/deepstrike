from __future__ import annotations
import inspect
import json
from collections.abc import AsyncIterable
from typing import Any, Callable
from deepstrike._kernel import ToolSchema

ToolChunk = str | dict[str, Any]


class RegisteredTool:
    def __init__(self, fn: Callable, schema: ToolSchema):
        self.fn = fn
        self.schema = schema

    async def __call__(self, **kwargs):
        result = self.fn(**kwargs)
        if inspect.isawaitable(result):
            result = await result
        if isinstance(result, AsyncIterable):
            return result
        return str(result)


def _schema_for(fn: Callable) -> ToolSchema:
    hints = fn.__annotations__.copy()
    hints.pop("return", None)
    py_to_json = {int: "integer", float: "number", bool: "boolean", str: "string"}
    properties = {
        name: {"type": py_to_json.get(typ, "string")}
        for name, typ in hints.items()
    }
    return ToolSchema(
        name=fn.__name__,
        description=(fn.__doc__ or "").strip(),
        parameters=json.dumps({
            "type": "object",
            "properties": properties,
            "required": list(properties.keys()),
        }),
    )


def tool(fn: Callable) -> RegisteredTool:
    return RegisteredTool(fn, _schema_for(fn))


def streaming_tool(fn: Callable) -> RegisteredTool:
    return RegisteredTool(fn, _schema_for(fn))


def normalize_tool_chunk(chunk: ToolChunk) -> dict[str, Any]:
    return {"type": "text", "text": chunk} if isinstance(chunk, str) else chunk


def tool_chunk_text(chunk: ToolChunk) -> str:
    normalized = normalize_tool_chunk(chunk)
    return str(normalized.get("text", "")) if normalized.get("type") == "text" else ""


def validate_tool_arguments(schema_json: str, args: dict[str, Any]) -> str | None:
    try:
        schema = json.loads(schema_json)
    except Exception:
        return "invalid tool schema"
    return _validate_value(schema, args, "$")


def _validate_value(schema: dict[str, Any], value: Any, path: str) -> str | None:
    expected = schema.get("type")
    if expected == "object":
        if not isinstance(value, dict):
            return f"{path} must be object"
        for required in schema.get("required", []):
            if required not in value:
                return f"{path}.{required} is required"
        for key, child_schema in schema.get("properties", {}).items():
            if key in value:
                err = _validate_value(child_schema, value[key], f"{path}.{key}")
                if err:
                    return err
    elif expected == "array" and not isinstance(value, list):
        return f"{path} must be array"
    elif expected == "string" and not isinstance(value, str):
        return f"{path} must be string"
    elif expected == "number" and not isinstance(value, (int, float)):
        return f"{path} must be number"
    elif expected == "integer" and not isinstance(value, int):
        return f"{path} must be integer"
    elif expected == "boolean" and not isinstance(value, bool):
        return f"{path} must be boolean"
    enum_values = schema.get("enum")
    if enum_values is not None and value not in enum_values:
        return f"{path} must be one of enum values"
    return None
