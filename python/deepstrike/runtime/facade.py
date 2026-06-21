"""High-level facades for the two bread-and-butter cases (parity with the Node SDK's ``runAgent`` /
``runFanout``), so callers don't assemble ``RuntimeRunner`` + session log + execution plane +
``collect_text`` by hand.

- ``run_agent``  — one prompt, one model, the text back. The 90%-case single-agent call.
- ``run_fanout`` — run N tasks in parallel, then synthesize, from a stateless request handler. Drives the
                   kernel-gated DAG via the standalone ``run_workflow`` path (governed / resumable).

Reach for ``RuntimeRunner`` directly when you need streaming events, signals, memory, or governance hooks.
"""
from __future__ import annotations

import uuid
from typing import Any

from .runner import RuntimeRunner, RuntimeOptions, collect_text
from .execution_plane import LocalExecutionPlane
from .session_log import InMemorySessionLog
from ..types.agent import WorkflowSpec, WorkflowNodeSpec


async def run_agent(
    *,
    provider: Any,
    goal: str,
    system_prompt: str | None = None,
    tools: list | None = None,
    session_id: str | None = None,
    max_tokens: int = 32_000,
    max_turns: int | None = None,
    session_log: Any | None = None,
    execution_plane: Any | None = None,
) -> str:
    """Run a single agent to completion and return its final text."""
    plane = execution_plane
    if plane is None:
        plane = LocalExecutionPlane()
        if tools:
            plane.register(*tools)
    opts = RuntimeOptions(
        provider=provider,
        execution_plane=plane,
        session_log=session_log or InMemorySessionLog(),
        max_tokens=max_tokens,
        **({"max_turns": max_turns} if max_turns is not None else {}),
        **({"system_prompt": system_prompt} if system_prompt is not None else {}),
    )
    runner = RuntimeRunner(opts)
    return await collect_text(runner.run(goal=goal, session_id=session_id or f"agent-{uuid.uuid4()}"))


async def run_fanout(
    *,
    provider: Any,
    tasks: list,
    synthesize: str,
    worker_role: str = "explore",
    synthesis_role: str = "plan",
    session_id: str | None = None,
    max_tokens: int = 32_000,
    max_turns: int | None = None,
    session_log: Any | None = None,
    execution_plane: Any | None = None,
) -> dict[str, Any]:
    """Parallel fan-out -> synthesize over the kernel-gated DAG (standalone ``run_workflow``).

    Returns ``{"synthesis": str, "outputs": dict}``. Safe from a stateless handler — it bootstraps and
    tears down its own kernel.
    """
    opts = RuntimeOptions(
        provider=provider,
        execution_plane=execution_plane or LocalExecutionPlane(),
        session_log=session_log or InMemorySessionLog(),
        max_tokens=max_tokens,
        **({"max_turns": max_turns} if max_turns is not None else {}),
    )
    runner = RuntimeRunner(opts)
    spec = WorkflowSpec(
        nodes=[WorkflowNodeSpec(task=t, role=worker_role) for t in tasks]
        + [WorkflowNodeSpec(task=synthesize, role=synthesis_role, depends_on=list(range(len(tasks))))],
    )
    outcome = await runner.run_workflow(spec, **({"session_id": session_id} if session_id else {}))
    outputs = outcome.get("outputs", {})
    completed = outcome.get("completed", [])
    synthesis_id = f"wf-node{len(tasks)}"
    synthesis = outputs.get(synthesis_id)
    if synthesis is None and completed:
        synthesis = outputs.get(completed[-1])
    return {"synthesis": synthesis or "", "outputs": outputs}
