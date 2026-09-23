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

## Anthropic dynamic-workflow alignment (2026-09-23)

This alignment targets the product mechanism described in Anthropic's official Claude Code documentation, not a provider request format. Anthropic defines a dynamic workflow as a JavaScript orchestration script: the script decides what runs next, script variables hold intermediate results, `agent`, `parallel`, and `pipeline` compose fan-out and sequential work, while `phase` and `log` feed a progress view. The documented runtime also covers approval, isolation, concurrency/batch/total-agent limits, and replay after failure or interruption. See the [official workflow documentation](https://code.claude.com/docs/en/workflows).

### What DeepStrike already has

| Documented mechanism | DeepStrike substrate | Status |
| --- | --- | --- |
| Dynamic next-step planning | `submit_workflow_nodes`, `start_workflow` | Present, but model-tool driven rather than script-variable driven |
| Parallel fan-out | Kernel workflow batches and the runner's parallel driver | Present and quota-gated |
| Passing results downstream | DAG dependencies, `dependency_outputs`, reducers | Present |
| Verification patterns | classify, generate/filter, tournament, and verify templates | Present |
| Durable evidence and recovery | SessionLog, kernel journal, workflow completion records | Substrate present |
| Provider and tool I/O | Host runner executes; kernel adjudicates effects | Boundary is correct |

### Missing contracts

1. A `FileDynamicWorkflowStore` now saves and validates a `meta + source` artifact, but there is still no isolated script runtime exposing `agent`, `parallel`, `pipeline`, `phase`, `log`, and `args` to source text.
2. Session events record node completion, but there is no phase-level progress view with agent counts, tokens, elapsed time, and status.
3. Invocation fingerprints and pluggable replay stores now reuse a completed result for the same `runId + nodeId + prompt/options`, but kernel workflow replay still lacks failed-suffix invalidation, dependent descendants, and typed refusal when artifacts are missing.
4. Workflow launch has no pre-run approval card, raw-script inspection path, or advisory large-run warning.
5. `WorkflowNodeSpec.agent` is host metadata and `workflowNodeSpecToKernel` intentionally drops it. A dynamic script must resolve a target Agent at the host spawn boundary instead of inventing a kernel field.
6. Kernel quotas exist, but the article-level workflow contract does not yet model the default 16 concurrency, 256 ceiling, 4096 items per `parallel`/`pipeline`, 1000 agents per run, or size-guideline warnings.
7. The kernel append entry now reaches the runner, but `RuntimeRunner` still lacks the complete controller: `HostCommand::AppendWorkflowNodes`, `CanonicalRunnerRuntime.appendWorkflowNodes()`, and `RuntimeRunner.appendDynamicWorkflowNodes()` grow the same kernel operation; `DynamicWorkflowController` now provides a typed submission queue for asynchronous script-to-driver handoff, but `RuntimeRunner` does not yet own the full script lifecycle around it.

### First implementation slice

The branch now adds a provider-neutral `DynamicWorkflowExecutor` in `node/src/workflow/dynamic.ts`. It provides:

- `agent(prompt, options)`, which creates a one-node `WorkflowSpec` and enters the existing kernel through an injected `runWorkflow` host;
- bounded, order-preserving `parallel` (default 16, configurable up to 256) and sequential `pipeline`;
- `parallelAgents`, which groups declarative agent requests into kernel workflow batches while preserving order;
- `phase`, `log`, an immutable `args` snapshot, and typed progress;
- host guardrails for 1000 agents per run and 4096 items per batch, with kernel quotas remaining authoritative;
- `DynamicWorkflowScript` metadata/source types as the stable input contract for persistence and isolated execution.
- `InMemoryDynamicWorkflowReplayStore` / `FileDynamicWorkflowReplayStore` and invocation fingerprints as the replay cache boundary; fan-out preserves unchanged items and submits only fingerprint misses.
- `DynamicWorkflowController` provides a typed submission queue: the script pauses at a host workflow submission, an external driver consumes it through `nextSubmission()`, and the driver returns a `WorkflowOutcome` or failure through the id-addressed completion methods. This still does not mean that `RuntimeRunner` owns the complete lifecycle.

This slice deliberately does not execute arbitrary source text, grant scripts direct filesystem or shell access, or claim replay parity. It fixes the public vocabulary and kernel entry point first, then adds an isolated script VM and durable replay without creating a second execution authority.
