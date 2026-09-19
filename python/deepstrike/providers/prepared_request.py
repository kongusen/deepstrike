"""Freeze one provider request for measurement and dispatch without a second encoding pass."""
from __future__ import annotations

import math

from copy import deepcopy
from dataclasses import dataclass, fields, is_dataclass
from typing import Any, AsyncIterator, Awaitable, Callable, Literal

from .stream import StreamEvent


@dataclass(frozen=True)
class PreparedProviderRequest:
    scope: Literal["encoded_body", "adapter_input"]
    request: Any
    state: Any
    stream: Callable[[], AsyncIterator[StreamEvent]]
    count_tokens: Callable[[], Awaitable[Any]] | None = None

    def evidence(self) -> dict[str, Any]:
        return {"scope": self.scope, "request": self.request, "state": self.state}


def snapshot(value: Any) -> Any:
    """Copy logical adapter inputs, including pyo3 DTOs which cannot be pickled."""
    if isinstance(value, dict):
        return {key: snapshot(item) for key, item in value.items()}
    if isinstance(value, list):
        return [snapshot(item) for item in value]
    if isinstance(value, tuple):
        return tuple(snapshot(item) for item in value)
    if is_dataclass(value):
        return type(value)(**{item.name: snapshot(getattr(value, item.name)) for item in fields(value)})
    try:
        return deepcopy(value)
    except TypeError:
        from .request_plan import _json_value
        values = _json_value(value)
        return type(value)(**{key: snapshot(getattr(value, key)) for key in values})


def _validate_run_state(value: Any, ancestors: set[int] | None = None) -> None:
    """The fallback can bind JSON state only; opaque state needs explicit preparation."""
    if value is None or type(value) in (str, bool, int):
        return
    if type(value) is float and math.isfinite(value):
        return
    ancestors = set() if ancestors is None else ancestors
    if type(value) in (list, dict) and id(value) not in ancestors:
        ancestors.add(id(value))
        try:
            if isinstance(value, dict):
                if any(type(key) is not str for key in value):
                    raise ValueError("Custom provider state must be JSON; implement prepare_request for opaque state")
                children = value.values()
            else:
                children = value
            for child in children:
                _validate_run_state(child, ancestors)
            return
        finally:
            ancestors.remove(id(value))
    raise ValueError("Custom provider state must be JSON; implement prepare_request for opaque state")


def prepare_provider_request(provider: Any, context: Any, tools: list[Any], extensions: Any, state: Any) -> PreparedProviderRequest:
    prepare = getattr(provider, "prepare_request", None)
    if callable(prepare):
        return prepare(context, tools, extensions, state)
    _validate_run_state(state)
    frozen_context, frozen_tools = snapshot(context), snapshot(tools)
    frozen_extensions, frozen_state = snapshot(extensions), snapshot(state)
    from .request_plan import _json_value, _material_options
    material = {"context": _json_value(frozen_context), "tools": _json_value(frozen_tools),
                "options": _material_options(frozen_extensions or {})}
    replay = getattr(provider, "peek_provider_replay", None)
    if callable(replay):
        material["replay"] = [snapshot(replay(message.content, message.tool_calls or [])) for message in frozen_context.turns]

    async def stream():
        run_state = snapshot(frozen_state)
        if isinstance(state, dict) and isinstance(run_state, dict):
            state.clear()
            state.update(run_state)
            run_state = state
        async for event in provider.stream(frozen_context, frozen_tools, extensions=frozen_extensions, state=run_state):
            yield event

    count = getattr(provider, "count_tokens", None)
    async def count_tokens():
        import inspect
        kwargs = {"extensions": snapshot(frozen_extensions)}
        if "state" in inspect.signature(count).parameters:
            kwargs["state"] = snapshot(frozen_state)
        return await count(snapshot(frozen_context), snapshot(frozen_tools), **kwargs)
    return PreparedProviderRequest("adapter_input", material, frozen_state, stream, count_tokens if callable(count) else None)
