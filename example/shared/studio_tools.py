"""The Research Brief Studio's domain tools — Python mirror of shared/studio-tools.ts.

Local MOCKS over a small canned corpus: the *provider* is real, the *tools* are deterministic
stand-ins for a search index + document store. In Python a tool is a plain function decorated with
``@tool`` — the name comes from the function name, the description from the docstring, and the
parameter schema from the type hints. The model-facing contract (tool + arg names) matches the
TypeScript version exactly.
"""
from __future__ import annotations

import json

from deepstrike import tool

# The same on-topic corpus as the TS version (id / title / url / snippet / body / tags).
CORPUS = [
    {
        "id": "src-cache",
        "title": "Prompt caching keeps a long, byte-stable prefix",
        "url": "https://studio.local/sources/prompt-caching",
        "snippet": "A cache hit requires the prefix bytes to be identical turn to turn; volatile content belongs at the end.",
        "body": "Providers cache on an exact byte-prefix. The agent runtime keeps system + knowledge frozen at the front and pushes the volatile state turn to the tail, so the cacheable region grows monotonically. Reordering or rewriting an early message busts every downstream cache entry.",
        "tags": ["cache", "context", "performance"],
    },
    {
        "id": "src-memory",
        "title": "Agent memory is written through one governed gate",
        "url": "https://studio.local/sources/agent-memory",
        "snippet": "Every memory write passes a single syscall — validation, quota, and dedup come for free.",
        "body": "Rather than letting any code path append to a store, memory writes route through one WriteMemory syscall. That gate applies schema validation, a rolling-window rate limit, jaccard-similarity dedup, and an advisory relevance score. Retrieval at run-start seeds the decaying history so the model sees prior facts on turn one.",
        "tags": ["memory", "governance"],
    },
    {
        "id": "src-loop",
        "title": "A loop agent is not a new engine",
        "url": "https://studio.local/sources/loop-agent",
        "snippet": "One round is one bounded run; continuity is the transcript; the only new decision is 'what next'.",
        "body": "A self-pacing loop agent reuses the ordinary bounded run as its round. Continuity comes from replaying one stable session id, lifetime governance from a run group, and the sole new decision — continue / sleep / stop after a round — is a model-proposed, kernel-adjudicated pace verb. Silence means done.",
        "tags": ["loop", "orchestration"],
    },
    {
        "id": "src-workflow",
        "title": "Dynamic workflows schedule sub-agents as a governed DAG",
        "url": "https://studio.local/sources/dynamic-workflow",
        "snippet": "Fan-out workers, a deterministic reduce, a quality gate — every node spawn passes the same gate.",
        "body": "A workflow is a declarative DAG whose nodes lower to sub-agent run specs. Node kinds cover spawn, loop, classify, tournament, and a host-computed reduce. Every node spawn passes the one syscall gate (quota, quarantine, per-node caps), and a dependent node receives its dependencies' outputs — a DAG edge carries data, not just ordering.",
        "tags": ["workflow", "orchestration", "governance"],
    },
    {
        "id": "src-signals",
        "title": "External events reach the agent as attention signals",
        "url": "https://studio.local/sources/signals-reactive",
        "snippet": "A webhook ingests a signal; it drains at the next turn boundary through the attention policy.",
        "body": "A signal gateway ingests external events (webhooks, cron, completions). Each drains at a turn boundary and routes through the in-kernel attention policy, which decides whether to queue, soft-interrupt, or preempt. Recipient-addressed signals let one gateway serve many peer agents.",
        "tags": ["signals", "reactive"],
    },
]


@tool
def search(query: str) -> str:
    """Search the studio's source index for a query. Returns matching sources as a JSON list of {id, title, snippet, url}. Read a source's full text with read_source(id)."""
    terms = [t for t in query.lower().split() if t]
    scored = []
    for s in CORPUS:
        hay = f"{s['title']} {s['snippet']} {' '.join(s['tags'])}".lower()
        score = sum(1 for t in terms if t in hay)
        if score:
            scored.append((score, s))
    scored.sort(key=lambda p: p[0], reverse=True)
    hits = [{"id": s["id"], "title": s["title"], "snippet": s["snippet"], "url": s["url"]} for _, s in scored[:5]]
    return json.dumps(hits if hits else {"note": "no matching sources", "query": query})


@tool
def read_source(id: str) -> str:
    """Read the full text of one source by its id (from search results). Returns {id, title, url, body}."""
    src = next((s for s in CORPUS if s["id"] == id), None)
    if src is None:
        return json.dumps({"error": f"no source with id '{id}'", "known_ids": [s["id"] for s in CORPUS]})
    return json.dumps({"id": src["id"], "title": src["title"], "url": src["url"], "body": src["body"]})


def studio_tools():
    """The two studio tools every level starts from."""
    return [search, read_source]
