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
