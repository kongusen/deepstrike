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
    is_fatal: bool = False
    error_kind: str | None = None


@dataclass
class UsageEvent:
    type: str = "usage"
    total_tokens: int = 0
    # Full prompt size (uncached input + cache reads + cache writes) and output.
    input_tokens: int = 0
    output_tokens: int = 0
    # Cost breakdown (subset of input_tokens): reads bill ~0.1x, writes ~1.25x.
    cache_read_input_tokens: int = 0
    cache_creation_input_tokens: int = 0
    # I1: pro-rata per-slot attribution of cache_read_input_tokens (Anthropic only). None when the
    # provider doesn't honor cache_control. Shape: {"system": int?, "tools": int?, "messages": int?}.
    cache_read_input_tokens_by_slot: "dict | None" = None
    # Provider stop reason — "max_tokens" (Anthropic) / "length" (OpenAI) flag an output-cap
    # truncation that drives the kernel's max-output-tokens recovery. None when not reported.
    stop_reason: "str | None" = None


@dataclass
class DoneEvent:
    type: str = "done"
    iterations: int = 0
    total_tokens: int = 0
    status: str = "success"  # mirrors LoopResult.termination: completed/max_turns/token_budget/timeout/user_abort/error
    # ③ loop-agent pacing: the kernel-adjudicated after-round decision, snake_case dict
    # {"action", "delay_ms"?, "reason", "coerced_from"?}. None on non-loop runs.
    pace_decision: "Any | None" = None


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


@dataclass
class PermissionResponse:
    approved: bool
    responder: str | None = None
    reason: str | None = None


@dataclass
class PermissionResolvedEvent:
    type: str = "permission_resolved"
    call_id: str = ""
    tool_name: str = ""
    approved: bool = False
    responder: str = "host"
    reason: str | None = None


@dataclass
class ToolArgumentRepairedEvent:
    type: str = "tool_argument_repaired"
    call_id: str = ""
    name: str = ""
    original_arguments: str = ""
    repaired_arguments: str = ""


@dataclass
class ToolDeniedEvent:
    type: str = "tool_denied"
    call_id: str = ""
    tool_name: str = ""
    reason: str = ""


@dataclass
class ToolAuditFailedEvent:
    """A tool's ``ctx.audit(label, fn)`` best-effort side-effect threw. The tool itself completed
    successfully (no ``is_error`` flip, no retry); this event lets the host log / monitor that an
    audit-store / metrics-emit / non-essential persistence step failed."""
    type: str = "tool_audit_failed"
    call_id: str = ""
    name: str = ""
    label: str = ""
    error: str = ""


@dataclass
class EntropySample:
    """Kernel session-entropy measurement at a completed turn boundary. "Entropy" = session
    disorder: repetition, tool failures, rollbacks, context pressure. The component vector is
    the contract; ``score`` is a versioned default fold (``score_version``). All normalized
    components are in [0, 1]."""
    turn: int = 0
    score: float = 0.0
    score_version: int = 0
    rho: float = 0.0
    repeat_pressure: float = 0.0
    failure_rate: float = 0.0
    rollbacks_in_window: int = 0
    window_turns: int = 0


@dataclass
class EntropySampleEvent:
    """One kernel entropy sample, emitted once per completed turn (a heartbeat watch source:
    subscribe to drive an external supervisor without tailing the audit log)."""
    type: str = "entropy_sample"
    sample: EntropySample = field(default_factory=EntropySample)


@dataclass
class EntropyAlertEvent:
    """The opt-in kernel entropy watch tripped: ``score`` crossed ``threshold`` while armed and
    cooled down (see ``RunnerOptions.entropy_watch``). Correlate components via the same-turn
    ``entropy_sample`` event."""
    type: str = "entropy_alert"
    turn: int = 0
    score: float = 0.0
    threshold: float = 0.0


@dataclass
class WorkflowNodesSubmittedEvent:
    """R3-1: a workflow node's agent called the ``submit_workflow_nodes`` tool. The runner surfaces
    the requested nodes (it cannot apply them to the child's own kernel — the workflow lives in the
    parent kernel); ``run_workflow`` sends them to the parent kernel before the node's completion."""
    type: str = "workflow_nodes_submitted"
    nodes: list = field(default_factory=list)  # list[WorkflowNodeSpec]; untyped to avoid an import cycle


StreamEvent = Union[
    UsageEvent,
    ThinkingDelta,
    TextDelta,
    ToolCallEvent,
    ToolDeltaEvent,
    ToolSuspendEvent,
    ToolResultEvent,
    DoneEvent,
    ErrorEvent,
    PermissionRequestEvent,
    PermissionResolvedEvent,
    ToolArgumentRepairedEvent,
    ToolDeniedEvent,
    ToolAuditFailedEvent,
    EntropySampleEvent,
    EntropyAlertEvent,
    WorkflowNodesSubmittedEvent,
]
