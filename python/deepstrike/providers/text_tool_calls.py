"""Plan task 9.0 · tool calls a model wrote as text.

DeepSeek sometimes answers with its native tool-call markup (DSML) in the message text instead of
the API's ``tool_calls`` field, often in a degraded form (``｜｜DSML｜｜ parameter`` with doubled
bars and spaces). Left as text it enters history, the model imitates it on later turns, and the
run loops until its turn limit. The dialect layer keeps the markup out of the text: it parses it
into real tool calls, or — when it cannot — fails the call as a retryable protocol error so the
kernel's recovery asks again. Mirrors ``node/src/providers/text-tool-calls.ts``.
"""
from __future__ import annotations

import json
import re
import secrets
from typing import Any

from deepstrike.providers.protocol_adapter import ProtocolResponseError

_MARKUP_START = re.compile(r"<\s*/?\s*[｜|]+\s*DSML", re.IGNORECASE)
_TAG = re.compile(r"<\s*(/?)\s*[｜|]+\s*DSML\s*[｜|]+\s*([A-Za-z_]+)([^>]*)>", re.IGNORECASE)
_PARTIAL_START = re.compile(r"<\s*/?\s*(?:[｜|]+\s*(?:D(?:S(?:M)?)?)?)?$", re.IGNORECASE)
_PARTIAL_LOOKBACK = 24

#: Markup that does not parse fails like the Anthropic adapter's textual-tool-call guard (SPC-022).
TEXTUAL_TOOL_CALL_CODE = "textual_tool_call"
TEXTUAL_TOOL_CALL_MESSAGE = "Provider emitted a tool call as text instead of a native tool block"


def markup_start(text: str) -> int:
    """Index of the first DSML marker in ``text``, or -1."""
    match = _MARKUP_START.search(text)
    return match.start() if match else -1


def partial_markup_length(text: str) -> int:
    """How many trailing characters of ``text`` could begin a marker and must wait for more."""
    tail = text[-_PARTIAL_LOOKBACK:]
    at = tail.rfind("<")
    if at < 0:
        return 0
    return len(tail) - at if _PARTIAL_START.fullmatch(tail[at:]) else 0


def _attribute(attrs: str, name: str) -> str | None:
    match = re.search(rf'\b{name}\s*=\s*"([^"]*)"', attrs, re.IGNORECASE)
    return match.group(1) if match else None


def _parameter_value(raw: str, is_string: bool) -> Any:
    if is_string:
        return raw
    try:
        return json.loads(raw)
    except ValueError:
        return raw


def parse_text_tool_calls(markup: str) -> list[dict[str, Any]] | None:
    """The tool calls in ``markup`` as ``{"name", "arguments"}``, or None when it is not a complete,
    well-formed set of calls."""
    calls: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    parameter: tuple[str, bool, int] | None = None
    for match in _TAG.finditer(markup):
        close, tag, attrs = match.group(1), match.group(2).lower(), match.group(3)
        if tag == "invoke":
            if not close:
                name = _attribute(attrs, "name")
                if not name or current is not None or parameter is not None:
                    return None
                current = {"name": name, "arguments": {}}
            else:
                if current is None or parameter is not None:
                    return None
                calls.append(current)
                current = None
        elif tag == "parameter":
            if not close:
                name = _attribute(attrs, "name")
                if not name or current is None or parameter is not None:
                    return None
                parameter = (name, _attribute(attrs, "string") != "false", match.end())
            else:
                if current is None or parameter is None:
                    return None
                name, is_string, start = parameter
                current["arguments"][name] = _parameter_value(markup[start:match.start()], is_string)
                parameter = None
        # Wrappers (function_calls, tool_calls, a bare calls) carry nothing.
    if current is not None or parameter is not None or not calls:
        return None
    return calls


def text_tool_call_id() -> str:
    """A call id for a parsed call: unique, so read-backs and tool results never collide."""
    return f"call_text_{secrets.token_hex(5)}"


def textual_tool_call_error() -> ProtocolResponseError:
    return ProtocolResponseError(
        "openai-chat", TEXTUAL_TOOL_CALL_MESSAGE, provider_code=TEXTUAL_TOOL_CALL_CODE, retryable=True,
    )
