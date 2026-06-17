"""Tool error envelope + ``safe_tool`` decorator (Python parity for the Node ``tools/errors.ts``).

Three pieces:

- ``format_tool_error`` replaces the old ``str(exc)`` at SDK error sites: an ``Exception`` with
  no extra fields gives ``str(exc)`` (the message); an exception carrying ``code`` / ``hint`` /
  ``__cause__`` gives JSON; a non-exception value falls back to ``str(...)``.
- ``ToolError`` is the canonical "the tool wants to fail with a code+hint" exception. Throwing it
  from a ``safe_tool``-wrapped body produces ``{success:false, code, error, hint?}``.
- ``safe_tool`` wraps a function so plain-data returns become ``{success:true, data}`` and any
  raise becomes a fail envelope — opt-in equivalent of ``tool()`` for tools that want the
  stable structured shape on the wire instead of free-form strings.

The classic ``tool()`` factory is untouched; ``safe_tool`` is the migration path."""
from __future__ import annotations

import inspect
import json
from collections.abc import AsyncIterable
from typing import Any, Callable

from .registry import RegisteredTool, _schema_for


class ToolError(Exception):
    """Carries machine-readable ``code`` + optional ``hint`` alongside the message. ``safe_tool``
    converts a raised ``ToolError`` into ``{success:false, code, error, hint?}``. Code defaults
    to ``"internal"`` so a bare ``raise ToolError("...")`` still produces a usable envelope."""

    def __init__(self, message: str, *, code: str = "internal", hint: str | None = None,
                 cause: BaseException | None = None) -> None:
        super().__init__(message)
        self.code = code
        self.hint = hint
        if cause is not None:
            self.__cause__ = cause


def ok(data: Any = None) -> dict[str, Any]:
    return {"success": True} if data is None else {"success": True, "data": data}


def fail(code: str, error: str, hint: str | None = None) -> dict[str, Any]:
    env: dict[str, Any] = {"success": False, "code": code, "error": error}
    if hint is not None:
        env["hint"] = hint
    return env


def _is_envelope(v: Any) -> bool:
    return isinstance(v, dict) and isinstance(v.get("success"), bool)


def format_tool_error(err: Any) -> str:
    """Error-aware replacement for ``str(exc)`` / ``String(err)``.

    - ``Exception`` with no extra fields → ``str(exc)`` (the message).
    - ``Exception`` carrying ``code`` / ``hint`` / ``__cause__`` → JSON ``{message, name?, code?,
      hint?, cause?}`` — agents can branch on ``code``.
    - ``None`` / primitives / strings → ``str(...)`` unchanged.
    - Other objects → ``json.dumps(...)`` if possible (replaces the old ``"<X object at 0x...>"``)."""
    if err is None:
        return "None"
    if isinstance(err, str):
        return err
    if isinstance(err, BaseException):
        code = getattr(err, "code", None)
        hint = getattr(err, "hint", None)
        cause = getattr(err, "__cause__", None)
        if code is None and hint is None and cause is None:
            return str(err) or type(err).__name__
        payload: dict[str, Any] = {"message": str(err)}
        name = type(err).__name__
        if name not in ("Exception", "ToolError"):
            payload["name"] = name
        if code is not None:
            payload["code"] = code
        if hint is not None:
            payload["hint"] = hint
        if cause is not None:
            payload["cause"] = str(cause) if isinstance(cause, BaseException) else cause
        try:
            return json.dumps(payload, default=str)
        except Exception:
            return str(err) or type(err).__name__
    try:
        return json.dumps(err, default=str)
    except Exception:
        return str(err)


class _SafeRegisteredTool(RegisteredTool):
    """``RegisteredTool`` subclass with the safe-envelope try/except baked into ``__call__``.

    We can't reuse ``RegisteredTool``'s wrapper because that one stringifies the result; here we
    serialize via ``json.dumps`` so the model receives a proper envelope, not ``str({...})``."""

    async def __call__(self, _ctx: Any = None, **kwargs: Any) -> str:
        if self._wants_ctx:
            kwargs["ctx"] = _ctx
        try:
            result = self.fn(**kwargs)
            if inspect.isawaitable(result):
                result = await result
            if isinstance(result, AsyncIterable):
                # Streaming safe_tool: collect chunks, then envelope. Authors who want streaming
                # output should use the classic ``streaming_tool`` + raise convention; ``safe_tool``
                # is for the structured envelope path.
                chunks: list[str] = []
                async for chunk in result:
                    chunks.append(chunk if isinstance(chunk, str) else str(chunk))
                result = "".join(chunks)
            if _is_envelope(result):
                return json.dumps(result)
            return json.dumps(ok(result))
        except ToolError as e:
            return json.dumps(fail(e.code, str(e) or type(e).__name__, e.hint))
        except Exception as e:
            code = getattr(e, "code", None)
            hint = getattr(e, "hint", None)
            code_str = code if isinstance(code, str) else "internal"
            hint_str = hint if isinstance(hint, str) else None
            return json.dumps(fail(code_str, str(e) or type(e).__name__, hint_str))


def safe_tool(fn: Callable) -> RegisteredTool:
    """``tool()`` equivalent that wraps the body in a try/except and returns a stable
    ``{success, code, error, hint?}`` JSON envelope to the model. Drop-in: ``@safe_tool``
    instead of ``@tool``.

    The body may return plain data (auto-wrapped as ``ok(data)``), an envelope produced by
    ``ok()`` / ``fail()`` (passed through), or raise — ``ToolError`` becomes a fail envelope
    with the ``code`` and ``hint``; any other ``Exception`` becomes ``{success:false, code:
    err.code if str else "internal", error: str(err)}``."""
    return _SafeRegisteredTool(fn, _schema_for(fn))
