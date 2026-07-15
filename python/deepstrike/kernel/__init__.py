"""
deepstrike.kernel — internal runtime primitives.

These are the low-level building blocks used inside the SDK.
Most user code should not import from here directly; use the top-level
`deepstrike` package instead.
"""
from deepstrike._kernel import (
    RuntimeTask,
    LoopPolicy,
    LoopResult,
    SignalRouter,
    build_eval_messages,
    parse_verdict,
    verdict_output_schema,
    Verdict,
    SkillCandidate,
)

try:
    from deepstrike._kernel import KernelRuntime
except ImportError:
    KernelRuntime = None  # type: ignore[assignment]

__all__ = [
    "RuntimeTask",
    "LoopPolicy",
    "LoopResult",
    "KernelRuntime",
    "SignalRouter",
    "build_eval_messages",
    "parse_verdict",
    "verdict_output_schema",
    "Verdict",
    "SkillCandidate",
]
