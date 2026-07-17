from __future__ import annotations
import copy
import inspect
import json
import re
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
        return {"error": "invalid tool schema", "repaired": False, "args": args}
    state = {"repaired": False}
    wrapper = {"root": args}
    error = _validate_value(schema, wrapper, "root", "$", state)
    # A oneOf/anyOf ROOT replaces the value with its accepted probe deep-copy — in-place mutation
    # of the caller's dict only covers non-union roots. Callers must use the returned "args".
    return {"error": error, "repaired": state["repaired"], "args": wrapper["root"]}


def _validate_value(schema: dict[str, Any], parent: Any, key: Any, path: str, state: dict[str, bool]) -> str | None:
    value = parent[key]
    expected = schema.get("type")

    # 0. 多态联合 (oneOf / anyOf) —— 先于单一 type 分支匹配
    union = schema.get("oneOf") or schema.get("anyOf")
    if isinstance(union, list):
        for sub in union:
            # 先深拷贝再试：避免某分支的 auto-cast/裁剪部分改写后又失败，污染后续分支
            probe = {"v": copy.deepcopy(parent[key])}
            probe_state = {"repaired": False}
            if _validate_value(sub, probe, "v", path, probe_state) is None:
                parent[key] = probe["v"]  # 接受首个匹配分支(连同它内部的 repair)
                if probe_state["repaired"]:
                    state["repaired"] = True
                return None
        return f"{path} does not match any allowed shape"

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
        elif expected == "array":
            # coerceItemArray: LLMs commonly wrap array args in a single-key {"item": X} / {"items": X}
            # envelope, or emit a lone object where a one-element array was expected. Coerce both to a
            # list so per-element validation runs (yielding precise `$.path[i]…` errors) instead of a
            # blunt "must be array". Aligned with the str→number/bool casts above.
            if isinstance(value, dict):
                keys = list(value.keys())
                if len(keys) == 1 and keys[0] in ("item", "items"):
                    inner = value[keys[0]]
                    parent[key] = inner if isinstance(inner, list) else [inner]
                else:
                    parent[key] = [value]
                value = parent[key]
                state["repaired"] = True

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

            # 3a. 裁剪多余字段 —— 尊重 additionalProperties。
            # 缺省/False 维持旧的"裁剪"行为（所有现存工具都依赖它）；只有显式 True 或子 schema 才放行。
            properties = schema.get("properties", {})
            allowed_keys = set(properties.keys())
            additional = schema.get("additionalProperties")
            for obj_key in list(value.keys()):
                if obj_key in allowed_keys:
                    continue
                if additional is True:
                    continue  # 任意键放行：不校验、不裁剪
                if isinstance(additional, dict):
                    # 用子 schema 递归校验每个额外键的值（也会 auto-cast / 补默认）
                    err = _validate_value(additional, value, obj_key, f"{path}.{obj_key}", state)
                    if err:
                        return err
                    continue
                del value[obj_key]  # additionalProperties 缺省/False → 维持旧行为
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
        elif expected == "null" and value is not None:
            return f"{path} must be null"
    elif path == "$" and not isinstance(value, dict):
        return f"{path} must be object"

    enum_values = schema.get("enum")
    if enum_values is not None and value not in enum_values:
        return f"{path} must be one of enum values"
    # `const` is THE discriminator convention for oneOf variants (kind: {const: "edit"}). Without
    # it, union branches match on required+type alone and the WRONG branch can win — then its
    # allow-list strips keys the right branch declared.
    if "const" in schema:
        want = schema["const"]
        if isinstance(want, bool) or isinstance(value, bool):
            matches = value is want
        else:
            matches = value == want
        if not matches:
            return f"{path} must equal the const value {json.dumps(want)}"
    # Constraint keywords, checked per the value's actual type (JSON Schema semantics: string
    # constraints ignore non-strings, etc.). Keywords outside this set (allOf, multipleOf,
    # uniqueItems, format, if/then/else, …) are ignored, not rejected.
    if isinstance(value, str):
        min_length = schema.get("minLength")
        if isinstance(min_length, int) and len(value) < min_length:
            return f"{path} must be at least {min_length} characters"
        max_length = schema.get("maxLength")
        if isinstance(max_length, int) and len(value) > max_length:
            return f"{path} must be at most {max_length} characters"
        pattern = schema.get("pattern")
        if isinstance(pattern, str):
            try:
                compiled = re.compile(pattern)
            except re.error:
                compiled = None  # author-side bad regex: skip, never fail the call
            if compiled is not None and compiled.search(value) is None:
                return f"{path} must match pattern {pattern}"
    elif isinstance(value, (int, float)) and not isinstance(value, bool):
        for keyword, ok in (
            ("minimum", lambda bound: value >= bound),
            ("maximum", lambda bound: value <= bound),
            ("exclusiveMinimum", lambda bound: value > bound),
            ("exclusiveMaximum", lambda bound: value < bound),
        ):
            bound = schema.get(keyword)
            if isinstance(bound, (int, float)) and not isinstance(bound, bool) and not ok(bound):
                op = {"minimum": ">=", "maximum": "<=", "exclusiveMinimum": ">", "exclusiveMaximum": "<"}[keyword]
                return f"{path} must be {op} {bound}"
    elif isinstance(value, list):
        min_items = schema.get("minItems")
        if isinstance(min_items, int) and len(value) < min_items:
            return f"{path} must have at least {min_items} items"
        max_items = schema.get("maxItems")
        if isinstance(max_items, int) and len(value) > max_items:
            return f"{path} must have at most {max_items} items"
    # `not`: probe on a deep copy so a matching (= rejected) subschema's repairs never leak out.
    not_schema = schema.get("not")
    if isinstance(not_schema, dict):
        probe = {"v": copy.deepcopy(value)}
        if _validate_value(not_schema, probe, "v", path, {"repaired": False}) is None:
            return f"{path} must not match the disallowed shape"
    return None
