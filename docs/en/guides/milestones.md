# Milestones

Milestones are the Agent OS **Acceptance State Machine**. They split long work into unlockable phases; each phase must produce evidence and pass a verifier before later phases or capabilities continue.

**Source code:**
- `crates/deepstrike-core/src/types/milestone.rs`
- `crates/deepstrike-core/src/scheduler/milestone.rs`
- Python: `python/deepstrike/types/agent.py`

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| Phase state | `MilestoneTracker` manages pending / passed / failed phase state |
| Capability unlock | `unlocks` describes later phases or capabilities opened by a pass |
| Acceptance evidence | `required_evidence` states what the verifier must see |
| Failure handling | Policy can require a verifier, terminate the run, or auto-pass in development |
| Process collaboration | Often composes with sub-agents, contracts, and harnesses for phased delivery |

A milestone is not checklist prose. It is a kernel-trackable acceptance state machine for long implementations, migrations, and releases that cannot complete in one step.

![Milestones Mechanisms](/milestones_mechanisms.svg)

## Concept

```python
@dataclass
class MilestonePhase:
    id: str
    criteria: list[str]
    unlocks: list[dict]       # unlocked capabilities / next phases
    verifier: dict | None     # verification config
    required_evidence: list[str]

@dataclass
class MilestoneContract:
    phases: list[MilestonePhase]
```

Sub-agents carry a `milestones` field; kernel `MilestoneTracker` manages the phase state machine.

---

## Level 1: AgentRunSpec with milestones

```python
from deepstrike import AgentRunSpec, AgentIdentity, MilestoneContract, MilestonePhase

spec = AgentRunSpec(
    identity=AgentIdentity(agent_id="builder", session_id="s1"),
    role="implement",
    goal="Implement feature in phases",
    milestones=MilestoneContract(phases=[
        MilestonePhase(id="design", criteria=["Design doc complete"]),
        MilestonePhase(id="impl", criteria=["Core logic implemented"], unlocks=[{"phase": "design"}]),
        MilestonePhase(id="test", criteria=["Tests pass"], unlocks=[{"phase": "impl"}]),
    ]),
)
```

---

## Level 2: Milestone policy

```python
RuntimeOptions(
    ...,
    milestone_policy="require_verifier",  # require_verifier | terminate | auto_pass
    on_milestone_evaluate=async_evaluate_fn,
)
```

| policy | Behavior |
|--------|----------|
| `require_verifier` | External verifier must confirm |
| `terminate` | Fail terminates the run |
| `auto_pass` | Dev mode auto-pass |

---

## Level 3: Check result feedback

```python
from deepstrike import milestone_check_pass, milestone_check_fail

# From SDK callback
milestone_check_pass("design")
# or
milestone_check_fail("impl", reason="Missing error handling")
```

The kernel receives a `milestone_result` event and applies the phase retry policy. At `max_attempts`,
`terminate` ends immediately; `rollback` restores the phase transaction once and then ends with
`milestone_exceeded`, rather than re-entering an already exhausted retry loop.

---

## Level 4: Combine with workflow

- Workflow nodes can attach `MilestoneContract` via `AgentRunSpec`
- Milestones gate **phases within one agent**; workflows gate **between agents** in a DAG
- Both compose: a workflow node can spawn a sub-agent with its own milestone contract

```python
WorkflowNodeSpec(
    task="Build feature X in phases",
    role="implement",
    isolation="worktree",
    # milestones flow from AgentRunSpec / run_spec on the spawned child
)
```

Host runner options:

```python
# python/deepstrike/runtime/runner.py
milestone_policy: MilestonePolicy | None = None
on_milestone_evaluate: Callable[[dict[str, Any]], Awaitable[Any] | Any] | None = None
milestone_contract: MilestoneContract | None = None
```

---

## Kernel behavior

- `MilestoneTracker` tracks phase state per sub-agent run
- Unlock dependencies form a DAG inside the agent; failed checks block downstream phases
- Verifier results are observations; policy decides terminate vs retry

---

## Further reading

- [Sub-Agents & Collaboration](./sub-agents-and-collaboration)
- [Harness & Eval](./harness-and-eval) — verifier implementation
