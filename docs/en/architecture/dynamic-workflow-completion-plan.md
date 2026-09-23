# Dynamic Workflow Completion Plan

## Scope

Complete the provider-neutral dynamic workflow control plane:

1. discover and distribute validated workflow artifacts;
2. execute artifact source inside a restricted VM boundary;
3. require an explicit pre-run approval decision and persist lifecycle events;
4. make replay depend on artifact, arguments, limits, and invocation records rather than only a node prompt fingerprint.

## Ordered slices

### Slice 1: Lifecycle and approval

- Add a typed approval request and lifecycle event sink to `DynamicWorkflowRunOptions`.
- Emit run, approval, phase, agent, log, completion, failure, and cancellation events.
- Reject before user code or kernel work starts when approval is denied.

Acceptance: denied runs submit zero workflow nodes; approved runs expose an ordered event stream; existing progress callbacks remain compatible.

### Slice 2: Isolated artifact execution

- Add a `node:vm` executor with a minimal context containing workflow operations and immutable args.
- Reject module loading, process access, network primitives, and dynamic code generation.
- Normalize source failures into a typed artifact execution error and route them through lifecycle termination.

Acceptance: a script can call `agent`, `parallel`, `pipeline`, `phase`, and `log`; forbidden capabilities fail; kernel submission still goes through the existing controller.

### Slice 3: Artifact discovery and distribution

- Add a catalog interface over one or more local roots.
- Validate names, metadata, source, digest, and origin on discovery.
- Provide a load-and-execute path that passes validated artifact identity into the run.

Acceptance: artifacts can be listed across roots, loaded by stable name, rejected on digest mismatch, and executed without bypassing the host controller.

### Slice 4: Replay completeness

- Persist run identity, artifact digest, args digest, limits, lifecycle events, and invocation records.
- Refuse replay when immutable run input changes.
- Reuse only completed records; preserve failed/cancelled tails for diagnostics without treating them as successful work.

Acceptance: matching artifact and inputs reuse completed work; changed source, args, limits, or run contract reject reuse; replay state survives process restart.

## Checkpoints

- After Slice 1: workflow tests, build, and controller tests pass.
- After Slice 2: VM escape tests, lifecycle tests, and dynamic workflow tests pass.
- After Slice 3: artifact store/catalog tests and docs drift pass.
- After Slice 4: replay tests, full Node suite, contracts verify, and docs drift pass.

## Implementation status

All four slices are implemented on the current branch:

- Slice 1: typed approval and ordered lifecycle events.
- Slice 2: restricted `DynamicWorkflowVmExecutor` with bounded source and execution time.
- Slice 3: multi-root artifact catalog, digest validation, bundle transport, and distribution.
- Slice 4: version-2 replay snapshots with input identity, event history, successful-result reuse, and failed/cancelled tails.

The VM is intentionally a host boundary. Strong adversarial isolation still requires an OS-level worker or sandbox, and lifecycle events remain exposed through the executor/replay store rather than a new cross-layer kernel wire format.

## Non-goals

- The VM is a host isolation boundary, not a replacement for OS-level sandboxing.
- Provider-specific workflow formats do not cross the kernel boundary.
- The kernel remains authoritative for effects, quotas, cancellation, and durable operation state.
