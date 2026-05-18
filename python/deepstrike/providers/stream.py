from __future__ import annotations
from dataclasses import dataclass, field
from typing import Union
import json


@dataclass
class ThinkingDelta:
    type: str = "thinking_delta"
    delta: str = ""


@dataclass
class TextDelta:
    type: str = "text_delta"
    delta: str = ""


@dataclass
class ToolCallEvent:
    type: str = "tool_call"
    id: str = ""
    name: str = ""
    arguments: dict = field(default_factory=dict)


@dataclass
class ToolDeltaEvent:
    type: str = "tool_delta"
    call_id: str = ""
    name: str = ""
    delta: str = ""
    chunk: dict | None = None


@dataclass
class ToolSuspendEvent:
    type: str = "tool_suspend"
    call_id: str = ""
    name: str = ""
    suspension_id: str = ""
    payload: dict | None = None


@dataclass
class ToolResultEvent:
    type: str = "tool_result"
    call_id: str = ""
    name: str = ""
    content: str = ""
    is_error: bool = False


@dataclass
class DoneEvent:
    type: str = "done"
    iterations: int = 0
    total_tokens: int = 0
    status: str = "success"  # mirrors LoopResult.termination: completed/max_turns/token_budget/timeout/user_abort/error


@dataclass
class ErrorEvent:
    type: str = "error"
    message: str = ""


@dataclass
class PermissionRequestEvent:
    type: str = "permission_request"
    call_id: str = ""
    tool_name: str = ""
    arguments: str = ""
    reason: str = ""


StreamEvent = Union[
    ThinkingDelta,
    TextDelta,
    ToolCallEvent,
    ToolDeltaEvent,
    ToolSuspendEvent,
    ToolResultEvent,
    DoneEvent,
    ErrorEvent,
    PermissionRequestEvent,
]
