# Feature Guides

Guides are the Agent OS runtime-plane manual. Each page first explains where the capability sits in the OS, then expands from minimal configuration into host / kernel boundaries, runtime events, and test entry points.

## Recommended Paths

| Goal | Reading order |
|------|---------------|
| Run a governed agent | [Execution Plane & Tools](./execution-plane-and-tools) → [Governance](./governance) → [Session, Replay & Recovery](./session-replay-and-recovery) |
| Long-context work | [Context Engineering](./context-engineering) → [Memory](./memory) → [Prompt Cache Design](../concepts/prompt-cache-design) |
| Multi-agent workflow | [Dynamic Workflows](./workflow) → [Sub-Agents & Collaboration](./sub-agents-and-collaboration) → [Structured Output & Reducers](./structured-output-and-reducers) |
| Production operation | [OS Profile & Runtime Snapshots](./os-profile-and-snapshots) → [Provider Routing](./provider-routing) → [Signals & Reactive](./signals-and-reactive) |
| Quality gates | [Harness & Eval](./harness-and-eval) → [Milestones](./milestones) → [Structured Output & Reducers](./structured-output-and-reducers) |

## Guide List

| Guide | Agent OS runtime plane | Main code entry |
|-------|------------------------|-----------------|
| [Execution Plane & Tools](./execution-plane-and-tools) | Tool / Execution Plane: runs external actions after kernel approval | `runtime/execution_plane.py` |
| [Context Engineering](./context-engineering) | Context VM: manages renderable working set, compression, and cache stability | `context/manager.rs` |
| [Skills](./skills) | Capability Plane: loads capability instructions on demand and narrows tool exposure | `context/skill_catalog.rs` |
| [Memory](./memory) | Memory Plane: turns short-lived state into governed durable knowledge | `memory/` |
| [Dynamic Workflows](./workflow) | Process Scheduler: decomposes goals into schedulable, governed sub-agent DAGs | `orchestration/workflow/` |
| [Structured Output & Reducers](./structured-output-and-reducers) | Deterministic Compute Plane: uses schemas and reducers to reduce LLM uncertainty | `runtime/output_schema.py` |
| [Governance](./governance) | Syscall Governance Plane: adjudicates permissions, quotas, and constraints before action | `governance/` |
| [Provider Routing](./provider-routing) | Provider Plane: resolves kernel `model_hint` values to host-side model providers | `providers/` |
| [Multimodal Input](./multimodal) | Content Plane: carries typed image/audio parts, weights their token cost, and serializes them per vendor | `providers/base.ts` |
| [Session, Replay & Recovery](./session-replay-and-recovery) | Event Log / Recovery Plane: records evidence and enables recovery and reproduction | `runtime/session_log.py` |
| [OS Profile & Runtime Snapshots](./os-profile-and-snapshots) | Runtime Policy / Observability Plane: summarizes profiles, policies, and dashboard state | `runtime/os_profile.py` |
| [Signals & Reactive](./signals-and-reactive) | Attention / Signal Plane: injects external events into `state_turn` and peer coordination | `signals/` |
| [Sub-Agents & Collaboration](./sub-agents-and-collaboration) | Process Isolation Plane: defines roles, isolation, contracts, and handoff | `collaboration/` |
| [Harness & Eval](./harness-and-eval) | Quality Gate Plane: wraps runs in judge, feedback, and retry loops | `harness/` |
| [Milestones](./milestones) | Acceptance State Machine: splits long work into gated acceptance phases | `scheduler/milestone.rs` |

## Guides vs Reference

Guides explain how to compose capabilities. Reference lists fields and parameters:

- [RuntimeOptions](../reference/runtime-options)
- [WorkflowNodeSpec](../reference/workflow-node-spec)
- [Python API](../reference/python-api)

## Tests as Examples

| Feature | Test file |
|---------|-----------|
| Tools / ExecutionPlane | `python/tests/test_streaming_tools.py`, `python/tests/test_worktree_isolation.py` |
| Memory | `python/tests/test_memory_syscall.py` |
| Workflow | `python/tests/test_workflow_drive.py` |
| Output schema / Reducer | `python/tests/test_output_schema.py`, `python/tests/test_workflow_reduce.py` |
| Signals | `python/tests/test_signal_addressing.py` |
| Reactive | `python/tests/test_reactive_session.py` |
| Governance | `python/tests/test_resource_quota.py` |
| Session / Replay | `python/tests/test_provider_replay.py`, `python/tests/test_replay_fixture.py` |
