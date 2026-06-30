# Sub-Agents & Collaboration

Sub-agents and collaboration are the Agent OS **Process Isolation Plane**. They split one agent's work into multiple role-bound subprocesses with isolation boundaries and context inheritance policies, then use contracts / handoff to manage outputs.

**Source code:**
- `python/deepstrike/types/agent.py` — `AgentRunSpec`
- `python/deepstrike/collaboration/` — `AgentPool`, `ContractDrivenHarness`
- Kernel: `crates/deepstrike-core/src/proc/`, `scheduler/state_machine/process.rs`

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| Process identity | `AgentRunSpec.identity` and parent-child lineage are written to the session log |
| Role boundary | explore / plan / implement / verify shape default prompts, tools, and context inheritance |
| Isolation boundary | shared / read_only / worktree / remote map to different execution planes and cwd policies |
| Capability boundary | `capability_filter` composes with Skills / Governance to control tool visibility |
| Handoff boundary | Contracts and `HandoffArtifact` turn subprocess output into parent-consumable evidence |

This layer makes "multi-agent" more than multiple LLM calls: it becomes a traceable, governable, recoverable process tree.

![Process Isolation & Sub-Agents Mechanisms](/collaboration_mechanisms.svg)

## Concept

### Key `AgentRunSpec` fields

| Field | Description |
|-------|-------------|
| `role` | explore / plan / implement / verify / custom |
| `isolation` | shared / read_only / worktree / remote |
| `context_inheritance` | none / system_only / full |
| `capability_filter` | Allowed tool kind / id |
| `milestones` | Phased acceptance contract |

### Isolation modes

| isolation | Behavior |
|-----------|----------|
| `shared` | Share parent context (default) |
| `read_only` | Read-only inheritance; good for explore |
| `worktree` | Git worktree isolates cwd |
| `remote` | Remote VPC / sandbox plane |

---

## Level 1: Workflow nodes as sub-agents

Each `WorkflowNodeSpec` spawns an isolated sub-agent — see [Dynamic Workflows](./workflow).

```python
WorkflowNodeSpec(
    task="Security audit",
    role="verify",
    isolation="read_only",
    context_inheritance="system_only",
)
```

---

## Level 2: AgentPool role separation

```python
from deepstrike import AgentPool

pool = AgentPool()
pool.add("orchestrator", orchestrator_runner)
pool.add("executor", executor_runner)
pool.add("verifier", verifier_runner)
pool.configure_coordinator(orchestrator_runner.host_options, session_id="collab-1")

result = await pool.spawn(
    role="executor",
    goal="Implement feature X",
    parent_session_id="collab-1",
)
```

`configure_coordinator` enables the kernel spawn path; parent-child lineage is written to the session log.

---

## Level 3: Verification contract

```python
from deepstrike import (
    ContractBuilder, ContractDrivenHarness,
    AcceptanceCriterion, format_contract_for_system_prompt,
)

contract = ContractBuilder("feature-x").add_criteria([
    AcceptanceCriterion(id="tests", text="All unit tests pass", required=True),
]).build()

harness = ContractDrivenHarness(runner, contract, ...)
outcome = await harness.run(goal="Implement feature X")
```

Creator–verifier separation reduces self-preferential bias.

---

## Level 4: Handoff

```python
from deepstrike import HandoffBus, HandoffArtifact

bus = HandoffBus()
await bus.publish(HandoffArtifact(
    from_agent="executor",
    to_agent="verifier",
    content="Implementation complete. See diff in ...",
))
```

Handoff artifacts enter the knowledge partition for downstream agents.

### SubAgentHarnessConfig

Sub-agents automatically go through quality-gate retries:

```python
from deepstrike import SubAgentHarnessConfig

RuntimeOptions(
    ...,
    sub_agent_harness=SubAgentHarnessConfig(
        eval_provider=judge_provider,
        max_attempts=3,
    ),
)
```

Worktree isolation is configured on the host runner:

```python
# RuntimeOptions (python/deepstrike/runtime/runner.py)
worktree_manager: Any = None  # isolation: "worktree" sub-agents run inside a git worktree
```

---

## Kernel behavior

- Sub-agent spawn is a kernel syscall gated by `ResourceQuota` and sandbox profile
- Role defaults set isolation and context inheritance per spawn
- Parent session log records lineage for replay and resume

---

## Further reading

- [Harness & Eval](./harness-and-eval)
- [Milestones](./milestones)
- [Roles & Isolation](/en/concepts/roles-and-isolation)
