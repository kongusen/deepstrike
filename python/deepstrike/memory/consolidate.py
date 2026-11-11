"""Plan task 9.4 · offline consolidation is an ordinary run, not a kernel mechanism.

Between sessions, a host may ask a model to reconcile its long-term memory against what actually
happened. That is just an agent run: the model reads memory with ``memory``, searches the session
transcripts with ``transcript_search``, and proposes corrected or missing records with
``propose_memory``, which the kernel admits like any other proposal. When it runs and what it
costs is the host's decision; the kernel does not know consolidation exists.
Mirrors ``node/src/memory/consolidate.ts``.
"""
from __future__ import annotations

import dataclasses
import json
import re
from dataclasses import dataclass
from typing import Any, Awaitable, Callable

from deepstrike._kernel import ToolSchema
from deepstrike.tools.registry import RegisteredTool

TRANSCRIPT_SEARCH_TOOL_NAME = "transcript_search"
_SNIPPET_CHARS = 240

DEFAULT_CONSOLIDATION_GOAL = (
    "Reconcile long-term memory with what actually happened in the earlier sessions. Read the "
    "memory records that matter (memory), and check each against the transcripts "
    "(transcript_search). Where the transcripts contradict a record, propose a corrected record "
    "with the same name; where they show a rule or lesson memory lacks, propose it; finish by "
    "proposing one index record that says which records cover what. Cite the session and turn a "
    "record rests on in its content. Reply with a short list of what you proposed."
)


@dataclass(frozen=True)
class TranscriptLine:
    session_id: str
    seq: int
    turn: int
    #: Where the text came from: the goal, the model's text, or a tool result (with its call).
    source: str
    text: str


def _text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if value is None:
        return ""
    return json.dumps(value)


def _field(value: Any, name: str) -> Any:
    """A field of a dict or an object (session events carry kernel types such as ``ToolCall``)."""
    if isinstance(value, dict):
        return value.get(name)
    return getattr(value, name, None)


def _event(entry: Any) -> tuple[int, dict]:
    if isinstance(entry, dict):
        return int(entry.get("seq", 0)), entry.get("event", {})
    return int(getattr(entry, "seq", 0)), getattr(entry, "event", {})


def transcript_lines(session_id: str, entries: list[Any]) -> list[TranscriptLine]:
    """The searchable lines of one session's events: the goal, the model's text and its calls,
    and every tool result labeled with the call that produced it."""
    calls: dict[str, str] = {}
    lines: list[TranscriptLine] = []
    for entry in entries:
        seq, event = _event(entry)
        turn = event.get("turn") if isinstance(event.get("turn"), int) else 0
        kind = event.get("kind")
        if kind == "run_started":
            lines.append(TranscriptLine(session_id, seq, turn, "goal", _text(event.get("goal"))))
        elif kind == "llm_completed":
            for call in event.get("tool_calls") or []:
                calls[_text(_field(call, "id"))] = f"{_text(_field(call, 'name'))} {_text(_field(call, 'arguments'))}"
            said = _text(event.get("content"))
            if said.strip():
                lines.append(TranscriptLine(session_id, seq, turn, "model", said))
        elif kind == "tool_completed":
            for result in event.get("results") or []:
                call_id = _text(_field(result, "call_id"))
                source = f"{call_id} {calls.get(call_id, 'tool')}"
                lines.append(TranscriptLine(session_id, seq, turn, source, _text(_field(result, "output"))))
    return lines


def _terms(query: str) -> list[str]:
    seen: list[str] = []
    for term in re.split(r"[^\w./-]+", query.lower()):
        if len(term) > 1 and term not in seen:
            seen.append(term)
    return seen


def search_transcripts(lines: list[TranscriptLine], query: str, limit: int = 8) -> list[TranscriptLine]:
    """Lines ranked by how many query terms they contain (ties: earlier first), each cut to a
    snippet around its first hit. Deterministic."""
    wanted = _terms(query)
    if not wanted:
        return []
    scored = []
    for index, line in enumerate(lines):
        folded = line.text.lower()
        hits = [term for term in wanted if term in folded or term in line.source.lower()]
        if hits:
            scored.append((-len(hits), index, line, max(0, folded.find(hits[0]))))
    scored.sort(key=lambda item: (item[0], item[1]))
    out = []
    for _, _, line, first in scored[: max(1, limit)]:
        start = max(0, first - _SNIPPET_CHARS // 2)
        snippet = line.text[start : start + _SNIPPET_CHARS]
        prefix = "…" if start > 0 else ""
        suffix = "…" if start + _SNIPPET_CHARS < len(line.text) else ""
        out.append(dataclasses.replace(line, text=f"{prefix}{snippet}{suffix}"))
    return out


def transcript_search_tool(
    session_log: Any,
    session_ids: list[str] | Callable[[], list[str] | Awaitable[list[str]]],
    max_results: int = 8,
) -> RegisteredTool:
    """A host tool that searches session transcripts by keyword. Each result names its session,
    turn and source (the goal, the model, or ``call_id tool arguments``), so a model can cite it."""

    async def transcript_search(query: str, session_id: str = "", limit: int = 0) -> str:
        ids = session_ids() if callable(session_ids) else session_ids
        if hasattr(ids, "__await__"):
            ids = await ids  # type: ignore[misc]
        only = [session_id] if session_id else list(ids)
        lines: list[TranscriptLine] = []
        for sid in only:
            if sid not in ids:
                continue
            lines.extend(transcript_lines(sid, await session_log.read(sid)))
        cap = min(max_results, limit) if limit and limit > 0 else max_results
        found = search_transcripts(lines, query, cap)
        if not found:
            return "no matching lines"
        return "\n".join(f"[{line.session_id} · turn {line.turn} · {line.source}] {line.text}" for line in found)

    schema = ToolSchema(
        name=TRANSCRIPT_SEARCH_TOOL_NAME,
        description=(
            "Search the transcripts of earlier sessions: what was asked, what the model said, and "
            "what each tool returned. Use it to check a memory against what actually happened."
        ),
        parameters=json.dumps({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Words to look for."},
                "session_id": {"type": "string", "description": "Search only this session."},
                "limit": {"type": "integer", "description": f"Results to return (at most {max_results})."},
            },
            "required": ["query"],
        }),
    )
    return RegisteredTool(transcript_search, schema)


async def consolidate_memory(
    options: Any,
    session_ids: list[str],
    *,
    session_id: str | None = None,
    goal: str | None = None,
    max_results: int = 8,
) -> dict[str, str]:
    """Run one consolidation: an ordinary agent run with ``transcript_search`` added to its tools.
    ``options`` is the host's :class:`RuntimeOptions` (its memory store, candidate sink, memory
    runtime policy, agent id and scope unchanged); proposals go through the candidate ABI as usual."""
    from deepstrike.runtime.execution_plane import LocalExecutionPlane
    from deepstrike.runtime.runner import RuntimeRunner, collect_text

    plane = LocalExecutionPlane().register(
        transcript_search_tool(options.session_log, session_ids, max_results)
    )
    runner = RuntimeRunner(dataclasses.replace(
        options, execution_plane=plane, baseline_tool_ids=[TRANSCRIPT_SEARCH_TOOL_NAME],
    ))
    sid = session_id or f"consolidate-{len(session_ids)}"
    text = await collect_text(runner.run(goal=goal or DEFAULT_CONSOLIDATION_GOAL, session_id=sid))
    return {"session_id": sid, "text": text}
