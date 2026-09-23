# Type-driven boundary contracts analysis (v0.2.74)

This document records the architecture audit behind the type-driven boundary contract work. The central finding is that the runtime had several semantic crossings whose shapes were represented by anonymous records or duplicated inline lowering. Those crossings could compile while silently dropping policy, identity, or execution authority.

## Decisions

1. The live facade path is authoritative. The retired formal `agent-ir` path was removed together with its projections, public exports, conformance fixtures, and tests because this version has not shipped.
2. The host owns runtime bindings and the kernel owns execution facts. A host projection may carry a declaration into the kernel, but it must not manufacture kernel identity or authority.
3. Boundary contracts are registered by protocol family and checked from TypeScript types. Generated manifests are artifacts; the registry and typed adapter signatures are the sources of truth.
4. Kernel-to-host projections are read-only decodes. They may reconstruct host observations or runner DTOs, while kernel bookkeeping and authority remain on the kernel side.

## Current crossing families

| Family | Direction | Examples |
| --- | --- | --- |
| public-to-host | lower | Agent declaration to runtime options |
| host-to-kernel | project | Skills, capabilities, run policy, memory, signals, workflow definitions |
| kernel-to-host | decode | Kernel observations and workflow actions |
| host-provider | encode/decode | Provider request, stream, finish, and usage adapters |
| runtime-internal | materialize | Host-owned transformations that do not cross authority |

The contract checker now validates field names, renames, intentional drops, nested mappings, and the declared validation mode. Protocol manifests are generated from the resolved adapter signature and checked for drift.

## Workflow boundary

Workflow definitions enter the kernel through `workflowNodeSpecToKernel` and `workflowSpecToKernel`. Host-only node identity and agent bindings are dropped explicitly, control-flow kinds are derived, and snake_case kernel fields are validated.

The reverse action path now uses named `KernelWorkflowSpawnNode` and `KernelWorkflowBudget` DTOs. The runner consumes them through `workflowSpawnNodeFromKernel` and `workflowBudgetFromKernel`. Kernel bookkeeping fields such as `task_id`, `attempt_id`, `launch_token`, and `node_id` are intentionally dropped at this crossing. This replaces the previous anonymous `Record` action shape and removes unchecked casts from the workflow driver.

## Other subsystem findings

Memory trust is assigned when data crosses into the governed path; provenance is a path property rather than a field copied from model output. Signals have two host ingress paths but one shared drain and one shared kernel projection, preserving delivery correlation across normal, high, and critical urgency. Provider-specific wire behavior remains inside provider adapters and is covered by the provider protocol family.

## Validation status

The Node runtime, WASM conformance tests, Rust conformance tests, contract generation, and generated-artifact verification pass on the current branch. Python conformance requires a locally built extension; a system-installed extension can fail with a `PyInit_deepstrike` loader error unrelated to these boundary changes.
