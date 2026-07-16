<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="https://raw.githubusercontent.com/kongusen/deepstrike/main/docs/public/banner.png" alt="DeepStrike" width="420" />
  </a>
</p>

# DeepStrike Python SDK

Runtime framework built on a Rust kernel. The kernel owns loop control, context compression, governance, signal routing, and memory paging — the SDK owns all I/O (LLM calls, tool execution, disk, long-term memory).

Python is a first-class SDK for the **Agent OS native profile**: declarative governance and in-kernel signal routing are enabled by default on every run.

## Install

```bash
pip install deepstrike
```

Requires Python 3.10+. The Rust kernel is distributed as a pre-built wheel (`deepstrike._kernel`).

When developing against a local kernel build, rebuild the extension from the repo root:

```bash
maturin develop --manifest-path crates/deepstrike-py/Cargo.toml
```

---

## Quick start

```python
import asyncio
from deepstrike import (
    FileSessionLog,
    InMemorySessionLog,
    LocalExecutionPlane,
    OpenAIProvider,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
async def add(x: int, y: int) -> str:
    """Add two numbers."""
    return str(x + y)

plane = LocalExecutionPlane().register(add)
runner = RuntimeRunner(RuntimeOptions(
    provider=OpenAIProvider(api_key="sk-...", model="gpt-5-mini"),
    session_log=FileSessionLog(".deepstrike/sessions"),
    execution_plane=plane,
    max_tokens=4096,
))

asyncio.run(collect_text(runner.run(
    session_id="math-1",
    goal="What is 17 + 28?",
)))
# => "45"
```

### Recipes — the canonical entry points

Most apps need one of two shapes. Start with the facades; drop to `RuntimeRunner` for streaming, tools, signals, memory, or governance.

```python
from deepstrike import run_agent, run_fanout

# 1) Single agent — one prompt, one model, the text back.
answer = await run_agent(provider=provider, goal="What is 17 + 28?", tools=[add])

# 2) Parallel fan-out → synthesize over the kernel-gated DAG (safe from a stateless handler).
out = await run_fanout(
    provider=provider,
    tasks=["Summarize the auth module", "Summarize the data layer"],
    synthesize="Combine the findings into one summary.",
)
synthesis = out["synthesis"]
```

Same-session continuity is explicit via `session_id`:

```python
await collect_text(runner.run(session_id="chat-1", goal="My name is Ada."))
reply = await collect_text(runner.run(session_id="chat-1", goal="What is my name?"))
```

Use `InMemorySessionLog` for process-local sessions or `FileSessionLog` when replay should survive restarts. `wake(session_id)` resumes from the event log without inserting a duplicate `run_started` event.

Streaming:

```python
from deepstrike.providers.stream import TextDelta, ToolCallEvent, DoneEvent

async for event in runner.run(session_id="readme-1", goal="Summarize README.md"):
    if isinstance(event, TextDelta):
        print(event.delta, end="", flush=True)
    elif isinstance(event, ToolCallEvent):
        print(f"\n[→ {event.name}]")
    elif isinstance(event, DoneEvent):
        print(f"\ndone in {event.iterations} turns ({event.status})")
```

---

## Architecture

```text
┌─────────────────────────────────────────────────────────┐
│  RuntimeRunner (Layer 1.5)                              │
│  LLMProvider · ExecutionPlane · SessionLog · DreamStore │
└───────────────────────────┬─────────────────────────────┘
                            │ step(JSON event) ↔ actions / observations
┌───────────────────────────▼─────────────────────────────┐
│  deepstrike._kernel KernelRuntime                       │
│  P1 Syscall · P2 Sched · P3 MM · Proc · IPC             │
└─────────────────────────────────────────────────────────┘
```

The runner drives a single loop:

1. Kernel returns an **action** — `call_provider`, `execute_tool`, `evaluate_milestone`, or `done`.
2. SDK executes the action (stream LLM, run tools, call milestone verifier).
3. SDK feeds the result back as a kernel **event** (`provider_result`, `tool_results`, …).
4. Kernel **observations** (compression, page-out, spool, signals, …) are drained into `SessionLog`.

Kernel session events carry an optional `category` tag (`syscall` · `sched` · `mm` · `proc` · `ipc`) for diagnostics and OS snapshot rebuilds.

### What Agent OS gives you

The mechanisms above are not internal refactors — they change what you can build without custom runner code:

**Kernel-mediated runtime (M0–M4)**  
Tool calls, spawns, compression, and signals pass through one kernel gate with an explicit lifecycle (Ready / Running / Blocked / Suspended). You implement I/O; the kernel decides *when* and *whether*. Node, Python, and Rust share the same decision path, so `wake(session_id)` and cross-language tooling see consistent behavior.

**Longer, sturdier sessions (Layer-1 spool + semantic page-out)**  
Oversized tool results (> 50 KB) stay in context as a preview plus a `.spool/` reference — the model reads the full payload on demand via ordinary file tools. When pressure triggers semantic eviction, the SDK summarizes archived content into `DreamStore`. Long tasks survive token pressure instead of failing mid-run.

**Safety and governance by default (OS native profile)**  
Every run loads declarative `governance_policy` (deny / ask_user / rate-limit / param rules) and in-kernel signal routing (`signal_policy`, default queue 64). Dangerous tools, external interrupts, and approval flows are policy — not ad-hoc checks in your handlers.

**Long-term memory as syscalls (Phase-7)**  
`write_memory` and `query_memory` run outside the main tool loop: kernel validation before `DreamStore.commit`, search → `select_memories` → `memory_retrieval_result` on query. Failed writes emit `memory_validation_failed` for audit; good memory is durable without polluting history.

**Multi-agent and multi-signal orchestration**  
Sub-agents register in the kernel process table (`agent_process_changed`); parent runs suspend explicitly until `sub_agent_completed`. Signals get disposition (Interrupt / Queue / Observe / Dropped) in-kernel, so gateways, cron, and heartbeats compose with the main loop instead of racing it.

**Observable like an OS log**  
Spool, page-out, signals, processes, budgets, and memory events land in `SessionLog` with categories. Rebuild an OS snapshot (`page_out_count`, `spool_count`, `process_by_agent`, memory counters) from one event stream — replay still strips audit events when reconstructing LLM messages.

| You need… | Use… |
|---|---|
| Policy before tools run | `governance_policy` (default: allow-all native profile) |
| External interrupts | `signal_source` + in-kernel `signal_policy` |
| Spawn / memory-write quotas | `resource_quota` (`set_resource_quota`) |
| Huge tool output | Automatic Layer-1 spool; optional custom `result_spool` |
| Durable recall across runs | `DreamStore` + semantic `page_out` via `dream_summarizer` |
| Programmatic memory I/O | `runner.write_memory()` / `runner.query_memory()` |
| Debug / compliance | `SessionLog` events + OS snapshot helpers |

---

## Dynamic workflows

Instead of planning **and** executing a hard task in one long context window, hand the kernel a declarative DAG and let it spawn a fresh-context sub-agent per node. The kernel owns the control flow (gate · budget · suspend-on-join · resume); your SDK runs the agents. See the [top-level overview](../README.md#the-six-harness-patterns-as-first-class-kernel-nodes) for the full pattern catalog.

```py
from deepstrike import WorkflowSpec, WorkflowNodeSpec

# One fresh-context verifier per rule (no inherited author context → can't rubber-stamp),
# then a skeptic that reviews their flags. The kernel spawns the 3 verifiers as one gated
# batch, suspends on the join, and runs the skeptic once they complete.
outcome = await runner.run_workflow(WorkflowSpec(nodes=[
    WorkflowNodeSpec(task="Rule: money is integer cents — violated?", role="verify"),
    WorkflowNodeSpec(task="Rule: all errors propagate — violated?",    role="verify"),
    WorkflowNodeSpec(task="Rule: timestamps are UTC — violated?",       role="verify"),
    WorkflowNodeSpec(task="Skeptic: which flags are real violations?",  role="verify", depends_on=[0, 1, 2]),
]))
# => {"completed": ["wf-node0", …], "failed": []}
```

A node's `kind` selects the control-flow shape; the same executor drives them all, every spawn passing the syscall gate:

| Node kind | Behavior |
|---|---|
| spawn (default) | Run the node's agent once |
| loop (`max_iters`) | Re-run until the agent signals it's done, capped at `max_iters` |
| classify (`branches`) | The classifier's result selects one branch; the rest are pruned |
| tournament (`entrants`) | Generate N entrants, then a pairwise-judge bracket to one winner |
| reduce (`reducer`) | **Tokenless host-compute** — a pure function (`dedupe_lines` / `merge_json_arrays` / `concat` / `count`, or your own via the `reducers` option) over the node's dependency outputs |

### 0.2.11 capabilities

- **Runtime fan-out** — give a node the `submit_workflow_nodes_tool` and its agent can append nodes to the live DAG mid-run (true loop-until-done; one verifier per claim it discovers). Recorded and replayed on `resume_workflow`. Governance rejection fails the submitting node instead of acknowledging work that was never appended.
- **Quarantine, no escape** — set `trust="quarantined"` on a node that reads untrusted content; it's denied write-capable isolation in-kernel, and any nodes it submits are coerced to quarantined too (no privilege escalation).
- **Structured output** — set `output_schema` on a node; the runner instructs the agent, validates the result against the JSON-Schema subset, and re-runs once with the errors on mismatch. A node that never conforms fails (its dependents starve).
- **Budget as signal** — with a `max_workflow_nodes` / `max_concurrent_subagents` quota installed, each spawned node's goal carries its remaining headroom so a coordinator can size its fan-out to fit.

---

## Providers

Resource quotas are opt-in and flow through the same replayable kernel event ABI:

```python
from deepstrike import MemoryWriteRateLimit, ResourceQuota

runner = RuntimeRunner(RuntimeOptions(
    # ...
    resource_quota=ResourceQuota(
        max_concurrent_subagents=4,
        max_spawn_depth=2,
        memory_writes_per_window=MemoryWriteRateLimit(max_writes=20, window_ms=60_000),
    ),
))
```

The top-level package exports the base providers `OpenAIProvider`, `OpenAIResponsesProvider`, and
`AnthropicProvider`. **Every other backend is a factory function** in `deepstrike.providers`, with a
`protocol` argument where a backend speaks both the OpenAI- and Anthropic-compatible wire:

```python
from deepstrike.providers import deepseek, kimi, minimax

ds = deepseek(api_key="...")                          # OpenAI-compatible wire (default)
dsA = deepseek(api_key="...", protocol="anthropic")   # Anthropic-compatible wire
mm = minimax(api_key="...")                           # MiniMax defaults to the Anthropic wire
```

| Entry | Import from | Backend |
|-------|-------------|---------|
| `OpenAIProvider` / `OpenAIResponsesProvider` | `deepstrike` | OpenAI (and OpenAI-compatible) |
| `AnthropicProvider` | `deepstrike` | Anthropic Messages API |
| `deepseek` · `kimi` · `qwen` · `glm` · `minimax` · `gemini` · `ollama` | `deepstrike.providers` | the respective vendor (factory functions) |

All providers accept `retry_config` for exponential backoff and share a `CircuitBreaker`.

`extensions` are forwarded to the provider while SDK-owned structural fields remain protected.

---

## Context model (four slots)

The kernel renders context as four LLM API slots — only **history** is compressed.

| Slot | Source | Role |
|------|--------|------|
| `system_stable` | system partition | Identity, rules — never changes within a run |
| `system_knowledge` | knowledge partition | Preloaded memory, skill defs — low frequency |
| `turns[0]` | `task_state` + signals | Goal, plan, progress, compression log, runtime signals |
| `turns[1..N]` | history | Conversation transcript |

```python
runner = RuntimeRunner(RuntimeOptions(
    initial_memory=["User prefers chartreuse."],  # → Slot 2
    system_prompt="You are a helpful assistant.",  # → Slot 1
    # ...
))
```

- `memory(query)` / `knowledge(query)` meta-tool results → **history** (tool results)
- Inbound signals are routed by the in-kernel attention policy and rendered into **Slot 3**

Full reference: [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md)

---

## Runtime options

```python
from deepstrike import (
    DEFAULT_NATIVE_GOVERNANCE_POLICY,
    DEFAULT_SANDBOX_POLICY,
    validate_declarative_policy,
    AgentIdentity,
    AgentRunSpec,
)
from deepstrike.runtime import DEFAULT_NATIVE_SIGNAL_POLICY, PromptBudget
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=FileSessionLog(".deepstrike/sessions"),
    execution_plane=plane,

    # Scheduler budget
    max_tokens=128_000,
    max_turns=25,
    timeout_ms=60_000,

    # Agent OS native profile (defaults shown)
    governance_policy=DEFAULT_NATIVE_GOVERNANCE_POLICY,
    signal_policy=DEFAULT_NATIVE_SIGNAL_POLICY,  # SignalRouter queue size 64
    prompt_budget=PromptBudget(
        prompt_overhead_tokens=20,
        output_reserve_tokens=4096,
        safety_margin_tokens=256,
    ),

    # Host I/O
    extensions={"temperature": 0.1},
    skill_dir="./skills",
    knowledge_source=my_ks,
    signal_source=gw,
    dream_store=my_store,
    agent_id="my-agent",
    initial_memory=["..."],

    # Memory paging & compression (SDK-side I/O)
    compression_store=archive_store,
    dream_provider=dream_llm,
    dream_summarizer=my_dream_summarizer,  # semantic page_out → DreamStore

    # Sub-agents & milestones
    run_spec=AgentRunSpec(
        identity=AgentIdentity(agent_id="my-agent", session_id="session-1"),
        role="orchestrator",
        goal="...",  # overridden by run() goal on start_run
    ),
    milestone_contract=my_contract,
    milestone_policy="require_verifier",
    on_milestone_evaluate=my_verifier,
    sub_agent_harness=SubAgentHarnessConfig(eval_provider=eval_provider, max_attempts=3),

    # Governance UX (AskUser path)
    on_permission_request=lambda req: {"approved": True, "responder": "user"},
))
```

| Option | Purpose |
|--------|---------|
| `governance_policy` | Declarative deny / ask_user / rate-limit / param rules loaded into the kernel before `start_run` |
| `signal_policy` | Versioned in-kernel signal queue/TTL policy (default queue 64) |
| `prompt_budget` | Provider-envelope overhead, output reserve, and safety margin deducted from the context window |
| `on_permission_request` | Resolves `tool_gated` + `suspended` → kernel `resume` with approved/denied call IDs |
| `compression_store` | Writes archived messages on `compressed` observations |
| `dream_summarizer` | Summarizes `page_out { tier_hint: "semantic" }` into `DreamStore` during a run |
| `dream_provider` | Separate LLM for `dream()` idle consolidation (falls back to `provider`) |
| `result_spool` | Custom large-result spool (default: `.spool/` under cwd) |

Validate policies before starting a run:

```python
result = validate_declarative_policy(
    gov_policy=DEFAULT_SANDBOX_POLICY,
    signal_policy=DEFAULT_NATIVE_SIGNAL_POLICY,
)
assert result["valid"], result["errors"]
```

Rebuild an OS diagnostics snapshot from session events:

```python
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events

events = [e.event for e in await session_log.read(session_id)]
snap = rebuild_os_snapshot_from_session_events(events)
# snap["page_out_count"], snap["spool_count"], snap["signals"], …
```

---

## Large result spool (Layer 1)

When a single tool result exceeds **50 KB**, the kernel keeps a short preview in context and emits `large_result_spooled`. The SDK writes the full payload to `.spool/` under the process cwd and logs `spool_ref` in the session.

`LocalExecutionPlane` transparently resolves read-tool arguments that point at `.spool/` paths:

```python
# Kernel context shows a preview + spool reference.
# LLM calls read_file(path=".spool/abc123…") → full content returned.
```

No configuration is required. Pass a custom `result_spool` on `RuntimeOptions` to change the directory (see `tests/test_semantic_page_out_dream.py` and spool-related tests).

---

## Tools

```python
from deepstrike import tool, read_file

plane.register(tool(name="search", description="Search.", parameters=schema)(my_fn))
plane.register(read_file)     # built-in: read files (also resolves .spool/ refs)
plane.unregister("search")
```

Execution planes:

| Plane | Use case |
|-------|----------|
| `LocalExecutionPlane` | In-process tools (default) |
| `FilteredExecutionPlane` | Capability-filtered sub-agent tools |
| `ProcessSandboxPlane` | OS subprocess isolation |
| `McpProxyPlane` | MCP server tools |
| `RemoteVpcPlane` | Remote execution |

Mount capabilities on an active run:

```python
runner.mount_tool(schema)
runner.mount_skill("summarize", "Summarize text")
runner.unmount_capability("tool", "search")
```

---

## Skills

Set `skill_dir` — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=4096,
    skill_dir="./skills",
))
```

```markdown
---
name: summarize
description: Summarize text into 2-3 concise bullet points
when_to_use: When you need to condense long text
effort: 1
---
1. Identify the 2-3 most important points
2. Express each as a concise bullet
```

---

## Knowledge

Implement `KnowledgeSource` — the kernel injects a `knowledge` meta-tool. Runtime retrieval results land in **history** as tool results (single-use fact content that decays with compaction), not in the durable `knowledge` partition. Use `initial_memory` for durable preload into Slot 2.

For durable content at runtime use `runner.push_knowledge(content, key=..., pinned=...)` — a **keyed** entry upserts on a repeated key and `runner.remove_knowledge(key)` removes it, both applied at the next compaction/renewal boundary. `knowledge_budget_ratio` (default 0.25, 0 disables) caps the partition: over budget, the oldest unpinned, non-skill entries are evicted at boundaries while `pinned=True` entries survive. A loaded skill's body is pinned here as `skill:<name>` and unpinned by `runner.deactivate_skill(name)` or a `skill_lease_turns` expiry.

```python
from deepstrike import KnowledgeSource

class VectorSearch(KnowledgeSource):
    async def init(self) -> None:
        await vector_db.connect()

    async def retrieve(self, query: str, top_k: int = 5) -> list[str]:
        return await vector_db.search(query, top_k)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    knowledge_source=VectorSearch(),
))
```

---

## Memory

### WorkingMemory (SDK-side scratch pad)

`WorkingMemory` is an SDK helper — not the kernel working partition. Kernel task state renders into Slot 3 (`turns[0]`).

```python
from deepstrike import WorkingMemory

mem = WorkingMemory()
mem.set("step", 1)
mem.get("step")  # 1
mem.clear()
```

### DreamStore (long-term memory)

```python
from deepstrike import DreamStore

class MyStore(DreamStore):
    async def load_sessions(self, agent_id): ...
    async def load_memories(self, agent_id): ...
    async def commit(self, agent_id, result, existing): ...
    async def search(self, agent_id, query): ...  # -> list[MemoryRecall]
    async def save_session(self, data): ...

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    dream_store=MyStore(),
    agent_id="my-agent",  # enables memory meta-tool + semantic page-out archival
))
```

Three memory paths:

| Path | When | What happens |
|------|------|--------------|
| In-session `memory(query)` | LLM calls meta-tool | `DreamStore.search()` → history tool result |
| `initial_memory` | Run start | Injected into Slot 2 (`system_knowledge`) |
| Semantic `page_out` | Kernel evicts with `tier_hint: "semantic"` | SDK summarizes via `dream_summarizer` / `dream_provider` → `DreamStore.commit()` |
| `dream(agent_id)` | Explicit idle call | `IdlePipeline` batch-consolidates past sessions |

```python
import time
from deepstrike.providers.stream import DoneEvent

async for event in runner.dream("my-agent", now_ms=int(time.time() * 1000)):
    if isinstance(event, DoneEvent):
        print(event.dream_result)
```

Custom semantic summarizer:

```python
async def dream_summarizer(archived, ctx):
    return f"Long-term summary for action={ctx.get('action')}"

runner = RuntimeRunner(RuntimeOptions(
    # ...
    dream_store=MyStore(),
    agent_id="my-agent",
    dream_summarizer=dream_summarizer,
))
```

### Phase-7 memory syscalls (`write_memory` / `query_memory`)

```python
from deepstrike import MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope

scope = MemoryScope(tenant_id="acme", namespace="assistant")
await runner.write_memory(MemoryRecord(
    record_id="prefers-small-tests-v1",
    scope=scope,
    name="prefers-small-tests",
    kind="feedback",
    content="User prefers focused unit tests for SDK behavior.",
    description="Testing preference",
    provenance=MemoryProvenance(author="host", trust="user_asserted"),
    created_at=1,
    updated_at=1,
), session_id="my-session")

hits = await runner.query_memory(MemoryQuery(
    scope=scope,
    query="Need testing preferences",
    top_k=5,
    kinds=["feedback"],
), session_id="my-session")
```

Session events: `memory_written`, `memory_queried`, `memory_validation_failed`, `memory_retrieval_result`.

---

## Governance

### In-kernel declarative policy (preferred)

Every run loads `governance_policy` into the kernel via `load_governance_policy`:

```python
from deepstrike import DEFAULT_SANDBOX_POLICY
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule, GovernanceRateLimit

policy = GovernancePolicy(
    rules=[
        GovernancePolicyRule(pattern="read_file", action="allow"),
        GovernancePolicyRule(pattern="write_file", action="ask_user"),
        GovernancePolicyRule(pattern="*", action="deny"),
    ],
    rate_limits=[GovernanceRateLimit(tool="api_call", max_calls=10, window_ms=60_000)],
)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    governance_policy=policy,
    on_permission_request=lambda req: {"approved": True, "responder": "cli"},
))
```

- `deny` → tool rejected with `tool_denied`
- `ask_user` → `tool_gated` + `suspended`; resolve via `on_permission_request`, then kernel `resume`

Default when omitted: allow-all (`DEFAULT_NATIVE_GOVERNANCE_POLICY`).

### Standalone Governance class

`Governance` wraps the native governance evaluator for SDK-side use (tests, custom gates). It is **not** wired automatically into `RuntimeRunner` — use `governance_policy` for run-time enforcement.

```python
from deepstrike import Governance

gov = Governance("allow")
gov.add_permission_rule("danger.*", "deny")
gov.block_tool("rm_rf")
gov.evaluate("read_file", '{"path":"x"}')
```

### SDK PermissionManager

`PermissionManager` is a separate SDK-side permission layer for apps that manage their own approval UX outside the kernel loop.

```python
from deepstrike import PermissionManager, PermissionMode

pm = PermissionManager(PermissionMode.DEFAULT)
pm.grant("fs", "read")
pm.evaluate("fs", "read")
```

---

## Signals

Inbound signals are routed by the in-kernel attention policy (default queue size 64):

| Urgency | Typical disposition |
|---------|-------------------|
| `critical` / `high` | `interrupt_now` — may yield a new `call_provider` action |
| `normal` / `low` | `queue` — buffered; no action until dequeued |
| queue full | `dropped` |

```python
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeSignal
from deepstrike.runtime import DEFAULT_NATIVE_SIGNAL_POLICY

gw = SignalGateway()
gw.schedule(ScheduledPrompt(goal="standup", run_at_ms=target_time))
gw.ingest(RuntimeSignal(kind="alert", payload={}, urgency="normal"))

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    signal_source=gw,
    signal_policy=DEFAULT_NATIVE_SIGNAL_POLICY,
))

runner.interrupt()  # cooperative abort → kernel timeout path
gw.destroy()
```

Each routed signal produces a correlated `signal_delivery_disposed` session event (`category: "ipc"`).

---

## Sub-agents

Spawn isolated child agents through the kernel process table:

```python
from deepstrike import AgentRunSpec, AgentIdentity
from deepstrike.providers.stream import DoneEvent

async for event in runner.spawn_sub_agent(AgentRunSpec(
    identity=AgentIdentity(agent_id="researcher-1", session_id="child-session"),
    role="explore",
    goal="Find three sources on topic X",
    isolation="worktree",
)):
    if isinstance(event, DoneEvent):
        print(event.status)
```

Requires an active parent run (`run()` / `wake()` in progress). The kernel emits `agent_process_changed`; the default `SubAgentOrchestrator` runs the child with a filtered execution plane and feeds `sub_agent_completed` back.

---

## Attempt loop

```python
from deepstrike import (
    AttemptLoop, AttemptRequest, RuntimeAttemptBody, StopPolicy,
    LlmEvalJudge, SubAgentHarnessConfig,
)

loop = AttemptLoop(
    body=RuntimeAttemptBody(runner),
    judge=LlmEvalJudge(eval_provider),
    stop=StopPolicy(max_attempts=3),
)
outcome = await loop.run(AttemptRequest(goal="Write a haiku"))
print(outcome.run_status, outcome.outcome, outcome.verdict)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    sub_agent_harness=SubAgentHarnessConfig(eval_provider=eval_provider, max_attempts=3),
))
async for event in loop.stream(AttemptRequest(goal="Write a haiku")):
    if event.type == "completed":
        print(event.outcome.run_status, event.outcome.verdict)
```

The default carry policy keeps one session and injects judge feedback as context. Use
`fresh_with_feedback` or `fresh_with_digest(...)` only when attempt isolation is intentional.

---

## Stream events

Import from `deepstrike.providers.stream`:

| Class | Key fields |
|-------|------------|
| `TextDelta` | `delta` |
| `ThinkingDelta` | `delta` |
| `ToolCallEvent` | `id`, `name`, `arguments` |
| `ToolDeltaEvent` | `call_id`, `name`, `delta`, `chunk?` |
| `ToolSuspendEvent` | `call_id`, `name`, `suspension_id`, `payload?` |
| `ToolResultEvent` | `call_id`, `content`, `is_error` |
| `PermissionRequestEvent` | `tool_name`, `reason` |
| `DoneEvent` | `iterations`, `total_tokens`, `status` |
| `ErrorEvent` | `message` |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error` · `milestone_pending`

---

## Further reading

- [SDK OS parity matrix](../docs/sdk-os-parity.md)
- [Kernel ABI reference](../docs/reference/kernel-abi.md)
- [Context slots & compression](../docs/concepts/context-slots-compression.md)
