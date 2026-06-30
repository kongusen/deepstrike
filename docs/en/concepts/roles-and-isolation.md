# Roles & Isolation

DeepStrike does not treat sub-agent isolation as a prompt convention. It lowers isolation into a kernel-checkable execution contract. The main sources of truth are:

- `crates/deepstrike-core/src/types/agent.rs`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `crates/deepstrike-core/src/scheduler/tcb.rs`
- `crates/deepstrike-core/src/proc/mod.rs`
- `python/deepstrike/types/agent.py`

## Core Model

A host-side `AgentRunSpec` enters the kernel, becomes an `IsolationManifest`, lands on `Tcb.proc`, and is later projected into `AgentProcess` observations / SDK ABI.

```text
AgentRunSpec
  role + isolation + capability_filter
        │
        ▼
IsolationManifest::from_spec(...)
  agent_id · parent_session_id · role · isolation
  context_inheritance · permitted_capability_ids
        │
        ▼
Tcb { proc: Some(ProcInfo), caps, budget, state }
        │
        ▼
AgentProcess::from_tcb(...)
```

One important detail: regular `AgentRunSpec` does not send `context_inheritance` directly to the kernel. `IsolationManifest::from_spec` derives it from `role`. Workflow nodes do carry their own `context_inheritance` field and role defaults because DAG templates need stronger per-node control.

## Role

Rust core defines `AgentRole`; Python / Node expose string literals:

```python
KernelAgentRole = Literal["explore", "plan", "implement", "verify", "custom"]
```

| role | Meaning | Common host behavior |
|------|---------|----------------------|
| `explore` | Read, inspect, research, search | Usually minimal inheritance and `read_only` |
| `plan` | Plan, synthesize, orchestrate | Usually needs fuller context |
| `implement` | Modify code or produce implementation | Often needs `worktree` or write privileges |
| `verify` | Verify, audit, judge | Should avoid inheriting the author's context |
| `custom` | Host-defined responsibility | Conservative by default |

## Default Inheritance

For regular spawn, defaults come from `IsolationManifest::role_default_context_inheritance`:

| role | Default `ContextInheritance` |
|------|------------------------------|
| `explore` | `system_only` |
| `verify` | `system_only` |
| `plan` | `full` |
| `implement` | `full` |
| `custom` | `none` |

Workflow node defaults come from `role_defaults(role)` and are more workflow-security oriented:

| role | Default `AgentIsolation` | Default `ContextInheritance` |
|------|--------------------------|------------------------------|
| `explore` | `read_only` | `system_only` |
| `verify` | `read_only` | `none` |
| `plan` | `shared` | `full` |
| `implement` | `worktree` | `full` |
| `custom` | `shared` | `none` |

This is why verifier nodes typically do not inherit the author's context: the goal is to reduce self-preferential bias, not repeat the author's rationale.

## Isolation

```python
AgentIsolation = Literal["shared", "read_only", "worktree", "remote"]
```

| isolation | Kernel meaning | Host responsibility |
|-----------|----------------|---------------------|
| `shared` | Run in the normal parent execution domain | SDK chooses concrete cwd / tool plane |
| `read_only` | Read-only semantics for untrusted explore / verify | ExecutionPlane should avoid write tools or writable cwd |
| `worktree` | Requires an isolated working directory | Python `RuntimeOptions.worktree_manager` creates and cleans a git worktree |
| `remote` | Remote isolated execution | Host connects a remote sandbox / VPC / process sandbox |

The kernel stores declarative state only. It does not create worktrees, remote sandboxes, or filesystem ACLs. Real I/O isolation is SDK / ExecutionPlane work.

## Capability Filter

`AgentCapabilityFilter` narrows capabilities at spawn time:

```python
@dataclass
class AgentCapabilityFilter:
    allowed_kinds: list[str] = field(default_factory=list)
    allowed_ids: list[str] = field(default_factory=list)
```

Rust core applies it to the parent's current `CapabilityManifest`, then writes the result into `IsolationManifest.permitted_capability_ids` and child `Tcb.caps`.

| Field | Behavior |
|-------|----------|
| empty `allowed_kinds` | no kind restriction |
| empty `allowed_ids` | no id restriction |
| both non-empty | capability must match both kind and id |

It composes with:

- `RuntimeOptions.allowed_tool_ids`: static per-run tool profile
- Skill gating: allow-set declared by active skills
- Governance: allow / deny / gate at the syscall trap
- ResourceQuota: spawn, memory write, workflow growth, and related resource caps

## NodeTrust and Quarantine

Workflow nodes also carry `NodeTrust`:

```python
NodeTrust = Literal["trusted", "quarantined"]
```

`quarantined` means the node read untrusted input. The kernel enforces three constraints:

| Constraint | Implementation | Behavior |
|------------|----------------|----------|
| quarantine must be read-only | `scheduler/state_machine/workflow.rs` | a quarantined node requesting write-capable isolation is denied at spawn |
| taint propagation | `orchestration/workflow/run.rs` | nodes submitted by a quarantined submitter are coerced to quarantined |
| cross-boundary labeling | `scheduler/state_machine/process.rs` | quarantined child output crossing into a trusted parent is labeled untrusted-origin |

This is not a prompt hint; it is DAG-level no-privilege-escalation.

## TCB and AgentProcess

`AgentProcess` is not a second state store. `AgentProcess::from_tcb` reconstructs the process view from a child `Tcb`:

| TCB field | AgentProcess field |
|-----------|--------------------|
| `tcb.id` | `agent_id` |
| `tcb.proc.parent_session_id` | `parent_session_id` |
| `tcb.proc.role` | `role` |
| `tcb.proc.isolation` | `isolation` |
| `tcb.proc.context_inheritance` | `context_inheritance` |
| `tcb.caps` | `permitted_capability_ids` |
| `TaskState::Done(...)` | `joined` / `failed` |

Lineage, state, budget, and capability views all converge on `TaskTable`.

## Common Misreadings

| Misreading | Actual implementation |
|------------|-----------------------|
| role is only a prompt identity | role participates in default inheritance / isolation, workflow templates, and process views |
| kernel creates worktree isolation | kernel declares `AgentIsolation::Worktree`; SDK executes it |
| verifier always sees no parent context | regular spawn and workflow node defaults differ; workflow verify defaults to `none`, regular spawn to `system_only` |
| quarantined is just a label | kernel denies write-capable quarantine, propagates taint, and labels cross-boundary output |

## Further Reading

- [Sub-Agents & Collaboration](/en/guides/sub-agents-and-collaboration)
- [Governance](/en/guides/governance)
- [WorkflowNodeSpec](/en/reference/workflow-node-spec)
