from __future__ import annotations
import inspect
import json
from typing import Any, Callable
from deepstrike._kernel import ToolSchema


class RegisteredTool:
    def __init__(self, fn: Callable, schema: ToolSchema):
        self.fn = fn
        self.schema = schema

    async def __call__(self, **kwargs) -> str:
        result = self.fn(**kwargs)
        if inspect.isawaitable(result):
            result = await result
        return str(result)

def tool(fn: Callable) -> RegisteredTool:
    hints = fn.__annotations__.copy()
    hints.pop("return", None)
    py_to_json = {int: "integer", float: "number", bool: "boolean", str: "string"}
    properties = {
        name: {"type": py_to_json.get(typ, "string")}
        for name, typ in hints.items()
    }
    schema = ToolSchema(
        name=fn.__name__,
        description=(fn.__doc__ or "").strip(),
        parameters=json.dumps({
            "type": "object",
            "properties": properties,
            "required": list(properties.keys()),
        }),
    )
    return RegisteredTool(fn, schema)
