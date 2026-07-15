from __future__ import annotations

import json
from typing import Any

from deepstrike._kernel import Message
from deepstrike.memory.protocols import MemoryProvenance, MemoryRecord, MemoryScope, SessionData
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta


async def extract_session_memories(provider: Any, session: SessionData, scope: MemoryScope,
                                   system_prompt: str | None = None) -> list[MemoryRecord]:
    transcript = "\n".join(
        f"[{getattr(message, 'role', 'unknown').upper()}] {getattr(message, 'content', '')}"
        for message in session.messages
    )[:8000]
    context = RenderedContext(
        system_text="\n\n".join(filter(None, [
            system_prompt,
            "Extract durable, reusable facts from this completed session. Return only JSON; do not include transient progress or guesses.",
        ])),
        turns=[Message(
            role="user",
            content=(transcript + '\n\nReturn {"memories":[{"name":"stable-kebab-key",'
                     '"kind":"user|feedback|project|reference","content":"fact",'
                     '"description":"why durable","confidence":0.0,"links":[],"pinned":false,'
                     '"ttl_days":null,"evidence_refs":[]}]} with at most 10 items. '
                     'Return {"memories":[]} when nothing is durable.'),
            tool_calls=[],
        )],
    )
    output = ""
    create_state = getattr(provider, "create_run_state", None)
    state = create_state() if callable(create_state) else None
    async for event in provider.stream(context, [], extensions=None, state=state):
        if isinstance(event, TextDelta):
            output += event.delta
    return parse_extracted_memories(output, session, scope)


def parse_extracted_memories(output: str, session: SessionData, scope: MemoryScope) -> list[MemoryRecord]:
    cleaned = output.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.removeprefix("```json").removeprefix("```").removesuffix("```").strip()
    try:
        value = json.loads(cleaned)
    except (TypeError, ValueError):
        return []
    drafts = value.get("memories") if isinstance(value, dict) else None
    if not isinstance(drafts, list):
        return []
    records: list[MemoryRecord] = []
    for draft in drafts[:10]:
        if not isinstance(draft, dict):
            continue
        name = draft.get("name", "").strip() if isinstance(draft.get("name"), str) else ""
        kind = draft.get("kind")
        content = draft.get("content", "").strip() if isinstance(draft.get("content"), str) else ""
        if not name or kind not in {"user", "feedback", "project", "reference"} or not content:
            continue
        confidence = draft.get("confidence", 0.5)
        confidence = max(0.0, min(1.0, float(confidence))) if isinstance(confidence, (int, float)) else 0.5
        evidence_refs = [ref for ref in draft.get("evidence_refs", []) if isinstance(ref, str)] \
            if isinstance(draft.get("evidence_refs", []), list) else []
        links = [link for link in draft.get("links", []) if isinstance(link, str)] \
            if isinstance(draft.get("links", []), list) else []
        ttl_days = draft.get("ttl_days")
        records.append(MemoryRecord(
            record_id=f"{scope.tenant_id}:{scope.namespace}:{kind}:{name}",
            scope=scope, name=name, kind=kind, content=content,
            description=draft.get("description", "").strip() if isinstance(draft.get("description"), str) else "",
            provenance=MemoryProvenance(author="extraction", trust="untrusted",
                                        session_id=session.session_id, evidence_refs=evidence_refs),
            created_at=session.updated_at_ms, updated_at=session.updated_at_ms,
            confidence=confidence, links=links, pinned=draft.get("pinned") is True,
            ttl_days=ttl_days if isinstance(ttl_days, int) and ttl_days > 0 else None,
        ))
    return records
