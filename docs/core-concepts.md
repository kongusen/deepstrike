# Core Concepts

---

## Skills — *how to do things*

Skills are Markdown files stored in a `skills/` directory (configurable). The kernel automatically injects a `skill` meta-tool into every LLM call. When the model needs procedural guidance, it calls `skill(name="X")`, and the SDK loads the full file into the context window on demand.

This keeps the base context lean — you can have dozens of skills without paying for their tokens on every turn.

### File format

```markdown
---
name: debug
description: Step-by-step debugging guide
when_to_use: error, traceback, exception, stack trace
effort: 2
estimated_tokens: 800
---

## Debug protocol

1. Read the full traceback and identify the failing frame.
2. Check the last successful state before the failure.
3. Form a minimal hypothesis, then isolate and verify.
```

| Frontmatter field | Purpose |
| --- | --- |
| `name` | Identifier used in `skill(name="X")` calls |
| `description` | Shown to the model when it decides which skill to load |
| `when_to_use` | Comma-separated trigger keywords |
| `effort` | Relative effort estimate (1–5); higher = more turns expected |
| `estimated_tokens` | Token budget hint for context compression |

### Automatic skill extraction

`HarnessLoop` creates skills automatically. When a run passes the quality gate, `EvalPipeline` extracts the successful pattern and writes it as a new `.md` file. This closes the feedback loop: good solutions become reusable guides.

### SkillRegistry (Python / Node.js)

`SkillRegistry` lets you register, list, and load skills programmatically rather than from a directory:

```python
from deepstrike import SkillRegistry

registry = SkillRegistry(skills_dir="./skills")
registry.register(name="deploy", content="## Deploy protocol\n...")
agent.set_skill_registry(registry)
```

---

## Memory — *what was learned*

Memory is a two-phase pipeline that separates in-session retrieval from post-session consolidation.

Session continuity is a separate concern from memory. Reuse the same `sessionId` when you want an agent to continue the same conversation and see the earlier transcript; use `DreamStore` when you want distilled knowledge to survive across different sessions.

### Phase 1 — In-session retrieval

When the model needs context from prior sessions, it calls `memory(query)`. The SDK calls `DreamStore.search(query)` and injects the returned entries into the context window.

```text
model → tool call: memory(query="how did we handle auth last time?")
SDK  → dream_store.search("how did we handle auth last time?")
     → [MemoryEntry { content: "Used JWT with RS256 ...", relevance: 0.91 }, ...]
     → injected into context as tool result
```

### Phase 2 — Post-session consolidation ("dreaming")

After each session, call `agent.dream(agent_id)` to trigger the `IdlePipeline`:

1. The pipeline reads `SessionData` (full transcript + metadata)
2. An LLM synthesises key insights, decisions, and patterns
3. New entries are compared against existing `DreamStore` contents (dedup + conflict resolution)
4. Surviving entries are committed to `DreamStore`

```python
# Python — call after every session
result = await agent.run(goal)
await agent.dream("my-agent-id")
```

```typescript
// Node.js
const result = await agent.run(goal)
await agent.dream("my-agent-id")
```

```rust
// Rust
let result = agent.run(goal).await?;
agent.dream("my-agent-id").await?;
```

### DreamStore interface

Implement `DreamStore` to connect any storage backend (vector DB, Postgres, Redis, etc.):

```python
from deepstrike import DreamStore, DreamResult, MemoryEntry

class MyVectorStore(DreamStore):
    async def search(self, query: str, top_k: int = 5) -> list[MemoryEntry]:
        ...
    async def commit(self, entries: list[MemoryEntry]) -> None:
        ...
    async def delete(self, ids: list[str]) -> None:
        ...
```

### WorkingMemory

`WorkingMemory` is an in-session scratchpad for the current run only. It is not persisted to `DreamStore` and is not searchable across sessions. Use it for intermediate state within a single conversation.

---

## Knowledge — *external facts*

Knowledge is read-only external information the agent can query but never modify. Implement `KnowledgeSource` to connect any RAG backend, vector DB, API, or document store.

```typescript
// Node.js
import { KnowledgeSource } from "@deepstrike/sdk"

class CompanyWiki implements KnowledgeSource {
  async retrieve(query: string): Promise<string> {
    const docs = await vectorDB.search(query, { topK: 3 })
    return docs.map(d => d.content).join("\n\n---\n\n")
  }
}

agent.setKnowledgeSource(new CompanyWiki())
```

```python
# Python
from deepstrike import KnowledgeSource

class CompanyWiki(KnowledgeSource):
    async def retrieve(self, query: str) -> str:
        docs = await vector_db.search(query, top_k=3)
        return "\n\n---\n\n".join(d.content for d in docs)

agent.set_knowledge_source(CompanyWiki())
```

When the model calls `knowledge(query="...")`, the SDK calls `retrieve()` and injects the result as a tool result. The agent cannot write to knowledge — it is a one-directional source of truth.

**Memory vs Knowledge:**

| | Memory | Knowledge |
| --- | --- | --- |
| Updated by the agent | Yes (post-session) | No |
| Query mechanism | Semantic search over `DreamStore` | `KnowledgeSource.retrieve()` |
| Scope | Agent-specific, accumulated over time | Shared, externally managed |

---

## Harness — *quality control*

The harness system wraps agent sessions with evaluation and retry logic.

### SinglePassHarness

Runs the agent once and returns a scored `HarnessOutcome`. No retries.

```python
from deepstrike import SinglePassHarness, HarnessRequest

harness = SinglePassHarness(agent, quality_gate=0.8)
outcome = await harness.run(HarnessRequest(goal="Write a Python sort function"))
print(outcome.passed, outcome.score, outcome.feedback)
```

### HarnessLoop

Wraps a full agent session with LLM-as-judge retry:

```text
attempt 1 → agent.run(goal)
          → EvalPipeline: score=0.4  feedback="no error handling, no docstring"
attempt 2 → agent.run(goal + "\n\nPrevious feedback: " + feedback)
          → EvalPipeline: score=0.85 ✓ passed
          → SkillCandidate extracted → written to skills/sort_function.md
```

```typescript
// Node.js
import { HarnessLoop, HarnessRequest } from "@deepstrike/sdk"

const harness = new HarnessLoop(agent, { maxAttempts: 3, qualityThreshold: 0.8 })
const outcome = await harness.run(new HarnessRequest("Write a Python sort function"))
console.log(outcome.passed, outcome.score)
```

### EvalLoopHarness

Gives programmatic control over the evaluation loop — useful when you want to plug in a custom evaluator instead of the built-in LLM-as-judge:

```python
from deepstrike import EvalLoopHarness

class MyEval(EvalLoopHarness):
    async def evaluate(self, result, attempt) -> tuple[float, str]:
        # return (score, feedback)
        score = run_tests(result.content)
        return score, "tests failed" if score < 1.0 else ""
```

---

## Signals — *external interrupts*

`SignalGateway` is the entry point for all external events during a running session. Signals flow through the kernel's `SignalRouter`, which assigns a disposition based on urgency.

### Dispositions

| Disposition | Urgency | Behaviour |
| --- | --- | --- |
| `interrupt_now` | Critical | Stop immediately; discard the current turn |
| `interrupt` | High | Finish the current tool call, then stop |
| `queue` | Normal | Deliver the signal at the start of the next turn |
| `observe` | Low | Record the signal; do not interrupt the loop |
| `dropped` | — | Queue was full; backpressure applied |

### ScheduledPrompt

`ScheduledPrompt` is a built-in `SignalSource` that fires a prompt at a wall-clock timestamp. It deduplicates by goal+time, so replaying it is idempotent.

```python
from deepstrike import ScheduledPrompt, SignalGateway
import time

gateway = SignalGateway()
gateway.schedule(ScheduledPrompt(
    goal="Summarise today's activity log",
    run_at_ms=int(time.time() * 1000) + 60_000,  # 1 minute from now
))
agent.set_signal_gateway(gateway)
```

### Custom signal sources

```typescript
// Node.js — inject a webhook event
agent.injectSignal({
  type: "user_message",
  content: "Please stop what you're doing — priority changed.",
  urgency: "high",  // → disposition: interrupt
})
```

---

## Collaboration — *multi-agent coordination*

The collaboration layer lets you run multiple agents in coordinated roles without sharing their conversation histories. It builds on the primitives described in the [Collaboration guide](./collaboration.md).

### VerificationContract — *what correct looks like*

A `VerificationContract` defines success criteria before execution starts. It lives in the executor's `system` partition (never compressed) and is given to the verifier alongside the artifact.

```typescript
import { ContractBuilder } from "@deepstrike/sdk"

const contract = new ContractBuilder("report-v1", "Write a research report on X")
  .criterion("has-sources",      "Report cites at least 3 sources", { weight: 0.4 })
  .criterion("no-hallucination", "All claims traceable to sources",  { weight: 0.6 })
  .antiPattern("Do not fabricate citations")
  .build()
```

### AgentPool — *role-isolated instances*

`AgentPool` holds one Agent per role. The verifier never sees the executor's history; the executor never sees the verifier's audit.

```typescript
import { AgentPool } from "@deepstrike/sdk"

const pool = new AgentPool()
  .add("executor", executorAgent)   // full tools, large context
  .add("verifier", verifierAgent)   // no tools, low temperature
```

### CreatorVerifierMode — *the simplest multi-agent pattern*

```typescript
import { CreatorVerifierMode, HandoffBus } from "@deepstrike/sdk"

const mode = new CreatorVerifierMode(pool, { maxAttempts: 3 })
const outcome = await mode.run(contract)

console.log(outcome.success)              // true / false
console.log(outcome.handoff.driftRate24h) // ratio of failed required criteria

if (HandoffBus.requiresEscalation(outcome.handoff)) {
  // drift > 5% or blocked_on non-empty — pause autonomous delegation
}
```

### HandoffArtifact — *what has been proven*

Every transition between sprints or agent instances produces a `HandoffArtifact`. It carries not only a progress summary but also `contractStatus` (per-criterion verdicts) and `driftRate24h`, so the next agent knows what has been verified, not just what was attempted.

### TaskLane — *parallelism hints*

`TaskLane` on `RuntimeTask` tells the executor how to schedule work:

| Lane | Parallelism |
| --- | --- |
| `orchestrate` | Serial — produces contracts |
| `implement` | Serial — code / content generation |
| `retrieve` | Parallel — web search, knowledge queries |
| `verify` | Parallel, context-isolated |

See the full [Collaboration guide](./collaboration.md) for API details and model selection recommendations.

---

## Safety — *permission boundaries*

Every tool call passes through the `GovernancePipeline` before execution. This happens inside the kernel — the SDK cannot bypass it.

### Pipeline stages

```text
Permission → Veto → RateLimit → Constraint → Audit
```

| Stage | Purpose |
| --- | --- |
| `Permission` | Checks whether this call is allowed under the current `PermissionMode` and per-tool rules |
| `Veto` | Hard blocks (e.g. shell commands matching a deny pattern); cannot be overridden at runtime |
| `RateLimit` | Token-bucket per tool per time window |
| `Constraint` | Validates arguments (path sandboxing, size limits, schema checks) |
| `Audit` | Structured log of every call, decision, and outcome |

### PermissionMode

`PermissionMode` controls the default permission posture:

| Mode | Behaviour |
| --- | --- |
| `auto` | Allow all registered tools without asking |
| `confirm_sensitive` | Ask for approval on tools flagged as sensitive |
| `confirm_all` | Ask for approval on every tool call |
| `deny_all` | Block all tool calls |

```python
from deepstrike import PermissionManager, PermissionMode

pm = PermissionManager(mode=PermissionMode.CONFIRM_SENSITIVE)
pm.allow("read_file")      # always allow
pm.deny("delete_file")     # always deny
agent.set_permission_manager(pm)
```

### Handling PermissionRequestEvent

When the pipeline requires user approval, the SDK yields a `PermissionRequestEvent` and pauses:

```typescript
// Node.js
for await (const event of agent.runStreaming(goal)) {
  if (event.type === "permission_request") {
    const granted = await promptUser(
      `Allow ${event.toolName}(${JSON.stringify(event.arguments)})?`
    )
    agent.resolvePermission(event.callId, granted)
  } else if (event.type === "text_delta") {
    process.stdout.write(event.delta)
  }
}
```

```python
# Python
async for event in agent.run_streaming(goal):
    if event.type == "permission_request":
        granted = await ask_user(event.tool_name, event.arguments)
        await agent.resolve_permission(event.call_id, granted)
    elif event.type == "text_delta":
        print(event.delta, end="", flush=True)
```
