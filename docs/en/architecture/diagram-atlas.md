# System Diagram Atlas

This atlas is the visual map of DeepStrike `0.2.48`. Every diagram is generated from one shared design system and describes a specific ownership boundary or runtime mechanism. The source of truth is [`scripts/generate-architecture-svgs.mjs`](https://github.com/kongusen/deepstrike/blob/main/scripts/generate-architecture-svgs.mjs).

Run `node scripts/generate-architecture-svgs.mjs` from the repository root after changing the diagram specification.

## System Boundary

### Complete runtime map

Host-owned I/O surrounds a pure Rust control plane; Self-Harness v2 improves only a bounded profile for the next run.

![DeepStrike runtime mechanism](/readme_agent_os_map.svg)

### Layered architecture

Application intent, host user space, ABI v2, kernel primitives, and the durable evidence plane remain separate.

![Agent OS architecture](/agent_os_architecture.svg)

### One turn through the kernel

Reason, act, adjudicate, execute, observe, and delta are explicit state transitions. Dynamic denials remain visible results and do not roll back the turn.

![L-star loop and syscall trap](/agent_os_loop_flow.svg)

## Orchestration and Policy

### Workflow dataflow

One concrete typed pipeline shows how edges carry durable values from isolated agents through a zero-token reducer into writing and verification.

![Workflow DAG dataflow](/agent_os_workflow_dag.svg)

### Dynamic workflow vocabulary

Static loading, runtime growth, first-class control nodes, scheduler barriers, trust, budgets, and recovery form one mechanism.

![Dynamic workflow mechanisms](/workflow_mechanisms.svg)

### Governance funnel

Pre-exposure filtering and call-time gates ensure the model never executes an effect directly.

![Syscall governance funnel](/governance_pipeline.svg)

### Structured outputs and reducers

The kernel carries schema contracts; the host validates and retries boundedly; deterministic reducers run without an LLM.

![Structured output and reducers](/reducers_mechanisms.svg)

### Milestones

Long tasks advance through explicit evidence, evaluation, failure policy, and capability unlocks.

![Milestone state machine](/milestones_mechanisms.svg)

## Context, Capabilities, and I/O

### Context VM

Four context slots, pressure accounting, compression, residency, and content lifetimes replace an unbounded chat log.

![Context VM mechanisms](/context_vm_mechanisms.svg)

### Skills and capability gating

On-demand skill knowledge composes with host and manifest ceilings through intersection-only capability narrowing.

![Skills and capability mechanisms](/skills_mechanisms.svg)

### Memory lifecycle

Query, recall, write, retention, and promotion preserve the distinction between decaying history and durable host-owned memory.

![Memory mechanisms](/memory_mechanisms.svg)

### ExecutionPlane

Approved calls move through host hooks into local, worktree, sandbox, or remote execution, with streaming, suspension, and spooling.

![ExecutionPlane mechanisms](/execution_plane_mechanisms.svg)

### Provider routing

The kernel carries `modelHint`; the host chooses vendor, protocol, endpoint, runtime policy, and replay implementation.

![Provider routing mechanisms](/provider_routing_mechanisms.svg)

### Multimodal input

Typed image and audio parts participate in token pressure, provider serialization, SessionLog persistence, and crash recovery.

![Multimodal mechanisms](/multimodal_mechanisms.svg)

## Coordination and Isolation

### Signals and reactive sessions

Leased signal delivery feeds the kernel attention plane; blackboard events, reaction checkpoints, peers, and RunGroup share one governance domain.

![Signals and reactive mechanisms](/signals_mechanisms.svg)

### Sub-agent collaboration

Context, capabilities, isolation, contracts, and handoff artifacts define a child process whose authority cannot exceed its parent.

![Sub-agent isolation and collaboration](/collaboration_mechanisms.svg)

## Evidence, Recovery, and Quality

### Session replay and recovery

One append-only evidence stream supports audit, provider replay, workflow resume, OS snapshots, and public-ABI state reconstruction.

![Session replay and recovery](/session_replay_mechanisms.svg)

### Profiles and snapshots

An OS Profile configures policy, an OS Snapshot supports observability, a KernelSnapshot restores execution, and a ContextSnapshot restores context only.

![Profiles and snapshots](/snapshots_mechanisms.svg)

### Runtime reliability

Replay windows, snapshot bounds, bounded retries, fuses, cancellation, entropy, and budget checks keep recovery finite and observable.

![Runtime reliability mechanisms](/reliability_mechanisms.svg)

### Harness and evaluation

Ordinary harnesses judge and retry one output while reusing the same runtime, workflow, and evidence contracts.

![Harness and evaluation](/harness_eval_mechanisms.svg)

### Self-Harness v2

Scope-isolated evidence, whitelisted edits, capability ceilings, injection screening, held-out validation, and tiered promotion evolve the next run's profile.

![Self-Harness v2](/self_harness_mechanisms.svg)
