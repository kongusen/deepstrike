# Type-driven boundary contracts plan (v0.2.74)

This plan sequences the convergence work around executable boundaries. Each slice introduces named source and target types, moves the live runtime path behind a typed adapter, registers the crossing, and adds focused behavioral verification.

## Completed slices

- Extracted the live facade-to-runtime options adapter and made the declaration snapshot immutable.
- Isolated agent runners by session and resolved handoff targets explicitly at the host spawn boundary.
- Registered capability, run configuration, kernel projection, observation decode, memory, signal, and provider adapter families.
- Added generated manifests and a checker that verifies adapter signatures and declared field policy.
- Registered workflow host-to-kernel definition projections for both root start and dynamic submission.
- Removed the unreleased formal `agent-ir` surface across Node, WASM, Python, conformance fixtures, tests, and public exports.
- Typed workflow kernel-to-host actions with `KernelWorkflowSpawnNode` and `KernelWorkflowBudget`, plus explicit host projection adapters.

## Workflow checkpoint

The workflow action checkpoint is complete. The registry covers both directions, generated manifests are checked in, and the runner no longer casts kernel action nodes or budgets through `unknown`. Kernel bookkeeping is dropped by declared policy, while execution fields used by reducers, loops, classifiers, output schemas, dependencies, and per-node limits are projected explicitly.

## Next sequence

1. Continue replacing anonymous records at remaining semantic crossings where a host or kernel authority boundary is present.
2. Add runtime validators only where structural typing cannot express the required invariant; keep behavioral tests for session-level correlation and lifecycle guarantees.
3. Keep provider wire details inside provider adapters and avoid expanding the kernel vocabulary with vendor-specific fields.
4. Run the contract checker, generated-artifact verification, Node and WASM builds, and the cross-SDK conformance matrix before merging.

## Non-goals

This track does not add a compatibility layer for the retired formal IR, change the Rust kernel ABI solely to improve TypeScript naming, or turn runtime-internal transformations into artificial language boundaries.

## Anthropic dynamic-workflow implementation plan

The target is the provider-neutral product mechanism documented by Anthropic: a rerunnable JavaScript orchestration script whose variables hold intermediate results while the runtime executes agents in the background. The kernel remains the only authority for spawn, quota, trust, cancellation, and durable facts.

### Slice A — public dynamic vocabulary (complete in this branch)

- [x] Add `DynamicWorkflowExecutor` with `agent`, `parallel`, `pipeline`, `phase`, `log`, and `args`.
- [x] Add named metadata, script artifact, limits, progress, and agent-result types.
- [x] Enforce default 16 concurrency, maximum 256 concurrency, 4096 items per batch, and 1000 agents per run before host submission.
- [x] Route every `agent()` call through the existing `runWorkflow` host callback.
- [x] Cover immutable args, ordering, phase/log progress, structured output, bounded fan-out, and limit rejection.

**Checkpoint:** The Node build and focused dynamic-workflow suite pass. This slice is an API/runtime adapter, not arbitrary source execution.

### Slice B — one kernel-owned dynamic run

- [ ] Introduce a `DynamicWorkflowController` owned by `RuntimeRunner` so one script run has one session/run id, one RunGroup reservation, and one kernel workflow operation.
- [ ] Compile `agent`/`parallel`/`pipeline` calls into dynamic DAG additions instead of starting a separate one-node workflow per call.
- [ ] Resolve `modelHint`, target Agent, tool access, and trust at the host spawn boundary before submission; never carry host-only `agent` metadata as an invented kernel field.

**Gate:** A parallel script shares one kernel quota ledger, one cancellation path, and one audit/session log; a denied append produces a typed rejection visible to the script.

### Slice C — isolated script artifact

- [ ] Persist `DynamicWorkflowScript` as a project/personal artifact with safe path checks and immutable run snapshots.
- [ ] Execute plain JavaScript in an isolated worker/VM with only the documented globals (`agent`, `parallel`, `pipeline`, `phase`, `log`, `args`).
- [ ] Reject module loading, direct filesystem/shell access, nondeterministic time/randomness, and mid-run user input at the script boundary.

**Gate:** The same script and args produce the same agent invocation sequence; the script cannot access host credentials or arbitrary process APIs.

### Slice D — progress, approval, and cost controls

- [ ] Add typed lifecycle events for phase start/end, agent start/end, log, approval, pause, resume, and cancellation.
- [ ] Add a pre-run approval request containing workflow metadata, phases, raw-script reference, size guideline, and projected cost.
- [ ] Add phase progress queries and advisory large-workflow warnings without weakening kernel quota enforcement.

**Gate:** A denied approval starts no child; pause/resume and cancellation leave a single terminal run state; progress survives remount.

### Slice E — deterministic replay and reuse

- [ ] Persist each agent invocation key, prompt fingerprint, input dependencies, result, and terminal status.
- [ ] Reuse completed results when the invocation fingerprint is unchanged; rerun the first changed/failed invocation and its descendants.
- [ ] Refuse relaunch when the referenced run artifact or saved result set is missing; never silently start over under a resume operation.

**Gate:** Editing one upstream prompt invalidates only its suffix; a failed middle fan-out reruns the documented suffix; a missing artifact returns a typed `nothing_to_resume` rejection.

### Slice F — authoring and distribution

- [ ] Add project and user workflow stores, name collision precedence, and plugin/package discovery.
- [ ] Validate literal `meta.name`/`meta.description` and phase title consistency before execution.
- [ ] Add `workflowSizeGuideline` configuration and a disable switch; keep these host policy inputs separate from kernel quota.

**Gate:** A saved script can be listed, inspected, launched with structured `args`, and disabled without changing the kernel ABI.
