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
        # M3/G4: a tool opts into the run context by declaring a ``ctx`` parameter; we pass it only
        # then, so existing tools (whose params are purely the tool args) are unaffected.
        try:
            self._wants_ctx = "ctx" in inspect.signature(fn).parameters
        except (TypeError, ValueError):
            self._wants_ctx = False

    async def __call__(self, _ctx=None, **kwargs):
        if self._wants_ctx:
            kwargs["ctx"] = _ctx
        result = self.fn(**kwargs)
        if inspect.isawaitable(result):
            result = await result
        if isinstance(result, AsyncIterable):
            return result
        return str(result)


def _schema_for(fn: Callable) -> ToolSchema:
    hints = fn.__annotations__.copy()
    hints.pop("return", None)
    # M3/G4: ``ctx`` is the runtime context, not a tool argument — never expose it in the schema.
    hints.pop("ctx", None)
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


def validate_tool_arguments(schema_json: str, args: dict[str, Any]) -> dict[str, Any]:
    try:
        schema = json.loads(schema_json)
    except Exception:
        return {"error": "invalid tool schema", "repaired": False}
    state = {"repaired": False}
    wrapper = {"root": args}
    error = _validate_value(schema, wrapper, "root", "$", state)
    return {"error": error, "repaired": state["repaired"]}


def _validate_value(schema: dict[str, Any], parent: Any, key: Any, path: str, state: dict[str, bool]) -> str | None:
    value = parent[key]
    expected = schema.get("type")

    # 1. 类型自动规整 (Auto-cast)
    if isinstance(expected, str):
        if expected == "boolean":
            if value == "true":
                parent[key] = True
                value = True
                state["repaired"] = True
            elif value == "false":
                parent[key] = False
                value = False
                state["repaired"] = True
        elif expected in ("number", "integer"):
            if isinstance(value, str):
                try:
                    num = float(value)
                    if expected == "integer":
                        if num.is_integer():
                            parent[key] = int(num)
                            value = int(num)
                            state["repaired"] = True
                    else:
                        parent[key] = num
                        value = num
                        state["repaired"] = True
                except ValueError:
                    pass

    # 2. 补默认值 (Default Injection)
    if expected == "object":
        if not isinstance(value, dict):
            return f"{path} must be object"
        properties = schema.get("properties", {})
        for prop_key, child_schema in properties.items():
            if prop_key not in value:
                if "default" in child_schema:
                    value[prop_key] = child_schema["default"]
                    state["repaired"] = True

    # 3. 校验并递归
    if isinstance(expected, str):
        if expected == "object":
            if not isinstance(value, dict):
                return f"{path} must be object"

            # 3a. 裁剪多余字段
            properties = schema.get("properties", {})
            allowed_keys = set(properties.keys())
            keys_to_remove = [k for k in value.keys() if k not in allowed_keys]
            if keys_to_remove:
                for k in keys_to_remove:
                    del value[k]
                state["repaired"] = True

            for required in schema.get("required", []):
                if required not in value:
                    return f"{path}.{required} is required"
            for prop_key, child_schema in properties.items():
                if prop_key in value:
                    err = _validate_value(child_schema, value, prop_key, f"{path}.{prop_key}", state)
                    if err:
                        return err
        elif expected == "array":
            if not isinstance(value, list):
                return f"{path} must be array"
            items_schema = schema.get("items")
            if items_schema:
                for i in range(len(value)):
                    err = _validate_value(items_schema, value, i, f"{path}[{i}]", state)
                    if err:
                        return err
        elif expected == "string" and not isinstance(value, str):
            return f"{path} must be string"
        elif expected == "number" and not isinstance(value, (int, float)):
            return f"{path} must be number"
        elif expected == "integer" and not isinstance(value, int):
            return f"{path} must be integer"
        elif expected == "boolean" and not isinstance(value, bool):
            return f"{path} must be boolean"
    elif path == "$" and not isinstance(value, dict):
        return f"{path} must be object"

    enum_values = schema.get("enum")
    if enum_values is not None and value not in enum_values:
        return f"{path} must be one of enum values"
    return None
