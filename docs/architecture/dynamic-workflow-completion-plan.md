# Dynamic Workflow Completion Plan

## Scope

Complete the remaining provider-neutral dynamic workflow control plane:

1. discover and distribute validated workflow artifacts;
2. execute artifact source inside a restricted VM boundary;
3. require an explicit pre-run approval decision and persist lifecycle events;
4. make replay dependent on the artifact, arguments, limits, and invocation records rather than only a node prompt fingerprint.

## Ordered slices

### Slice 1: Lifecycle and approval

- Add a typed approval request and lifecycle event sink to `DynamicWorkflowRunOptions`.
- Emit start, approval, phase, agent, log, completion, failure, and cancellation events.
- Reject before user code or kernel work starts when approval is denied.

Acceptance: denied runs submit zero workflow nodes; approved runs expose a complete ordered event stream; existing progress callbacks remain compatible.

### Slice 2: Isolated artifact execution

- Add a VM executor using `node:vm` with a minimal context containing only workflow operations, immutable args, and bounded timers.
- Reject module loading, process access, network primitives, and dynamic code generation.
- Normalize source failures into a typed artifact execution error and route them through lifecycle termination.

Acceptance: a script can call `agent`, `parallel`, `pipeline`, `phase`, and `log`; attempts to access `process`, `require`, or `eval` fail; kernel submission still goes through the existing controller.

### Slice 3: Artifact discovery and distribution

- Add a catalog interface over one or more local roots.
- Validate names, metadata, source, digest, and origin on discovery.
- Provide a load-and-execute path that passes the validated artifact identity into the run.

Acceptance: artifacts can be listed across configured roots, loaded by stable name, rejected on digest mismatch, and executed without bypassing the host controller.

### Slice 4: Replay completeness

- Persist run identity, artifact digest, args digest, limits, lifecycle events, and completed invocation records.
- Refuse replay when any immutable run input changes.
- Reuse only completed records from the matching run snapshot; preserve failed/cancelled tails for diagnostics without treating them as successful work.

Acceptance: same artifact and inputs reuse completed work; changed source, args, limits, or run contract cause a fresh run; replay state survives process restart.

## Checkpoints

- After Slice 1: Node workflow tests, build, and dynamic controller tests pass.
- After Slice 2: VM escape tests, lifecycle tests, and dynamic workflow tests pass.
- After Slice 3: artifact store/catalog tests and docs drift pass.
- After Slice 4: replay tests, full Node suite, contracts verify, and docs drift pass.

## Non-goals

- The VM is a host isolation boundary for workflow source, not a replacement for OS-level sandboxing.
- Provider-specific workflow formats do not cross the kernel boundary.
- The kernel remains authoritative for effects, quotas, cancellation, and durable operation state.
