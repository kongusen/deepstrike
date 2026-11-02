# Migration from 0.2.77 to 0.3.0

0.3.0 removes the context evidence layer and the Evolution Runtime. There is no compatibility
layer. This file grows as later stages of the
[context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md) land.

## Stored state

Journals and checkpoints written by 0.2.77 cannot be restored by 0.3.0. Provider-call effects no
longer carry `context_candidate`; checkpoints no longer carry `state_generation`, and workflow node
state no longer carries `kind` or `iterations_completed`. These types reject unknown fields. Finish
or abandon in-flight operations before upgrading.

Session logs can still be read. Old `context_prepared` entries are no longer part of the
registered vocabulary. The Rust `FileSessionLog` skips them, and new appends do not reuse their
seq. Node, Python, and WASM return them unchanged, with a `kind` outside the `SessionEvent` union.

## Behavior changes

- `plan` and `implement` sub-agents default to `system_only` inheritance instead of `full`. A child
  that relied on reading the parent transcript must say so: set `contextInheritance: "full"` on its
  spec, or put what it needs in its goal.
- Knowledge pushed mid-run (`page_in`, memory hits after the first call, async summaries) appears in
  the trailing state turn until the next compaction or renewal, not in system knowledge. Tests that
  look for it in system knowledge right after the push need to look in the state turn.
- Checkpoints carry new context fields (`prefix_sent`, `cache_invalidated_tokens`,
  `evicted_knowledge_keys`, `knowledge_regret`, `knowledge_wasted_tokens`, and `staged` on each
  knowledge slot). They default when absent, but 0.2.77 cannot read checkpoints written by 0.3.0.
- Memory recall is issued by the kernel. The first effect of a run with a memory store and scope
  is a `query_memory` whose `causation.kind` is `kernel_recall`; a custom host that executes
  effects itself must answer it like any memory query. `memory_query_result` hits should carry the
  record's `description` and `provenance.trust` so the index line can show them.
- A host that starts child runs itself should pass the spawn's `dependency_outputs` into the
  child (`dependencyOutputs` / `dependency_outputs` option, or the `add_dependency_output` host
  event before start). Without it, `read_result(call_id="dep:…")` reports that the run has no
  such dependency output.
- The session log gains `context_lifecycle_reported`, written once at the end of each run.

## Removed APIs

| Removed | Replacement |
| --- | --- |
| `createNativeContextPreparationAdapter`, `createContextPreparationAdapter`, `ContextPrepared` (Python: `create_native_context_preparation_adapter`, …) | None. The runner no longer prepares or records a context binding before each provider call. |
| `EvolutionRuntime`, `createEvolutionRuntimeAdapter`, `createNativeEvolutionRuntimeAdapter`, `EvaluationContextBinding`, `ContextExecutionInput`, `ContextPlan`, `ContextState`, artifact-set types | None. Compare configurations offline with your own evaluation runs. |
| `readSessionEvidence`, `createSharedEvidence`, `SharedEvidence*`, `Evidence*` (Rust: `read_session_evidence`, `project_shared_evidence`, `knowledge_identity`, …) | Read the session log and kernel journal directly. |
| `RuntimeOptions.artifactSetDigest` / `artifact_set_digest`; `OperationConfig.artifact_set_binding` | None. Remove the option. |
| `RunResult.evidence.contextBinding`, `RunResult.evidence.artifactSet` | `evidence.route` and `evidence.measurement` remain. |
| Rust `AgentRunResult.evidence`, `AgentEvidence` | Read `provider_attempt` and `prompt_measured` entries from the session log. |
| Kernel bindings `contextPrepareJson`, `contextVerifyJson`, `evolutionValidateJson` | None. |
| `deepstrike inspect\|verify\|replay\|fork` CLI, `VerifiableOperation`, `verifiableOperationJson`, verifiable-report modules | None. Restore verifies the chain and replays deterministically on its own. |

## Workflow control flow

Workflow nodes no longer have a kind. Each node is one agent run. The canonical kernel already
rejected `loop`, `classify`, `tournament` and `reducer` nodes at load, so a spec using them did not
run before either.

| Removed | Write instead |
| --- | --- |
| `loop: { maxIters }` | A loop in a `runDynamicWorkflow()` program, or a node that submits the next round with `submit_workflow_nodes`. |
| `classify: { branches }` | A classifier node that submits only the chosen branch with `submit_workflow_nodes`. |
| `tournament: { entrants }` | A `runDynamicWorkflow()` program that runs the entrants in parallel and judges pairs. |
| `reducer` / `builtinReducers` / `RuntimeOptions.reducers` | An ordinary function in the host program. |
| `genEval(worker, evaluate, maxIters, …)` | `genEval(worker, evaluate, extractSkillOnPass)`, driven by `AttemptLoop` for retries. |
| `memoryRecallTurn` / `memory_recall_turn` | None. The kernel writes the index line. |
| Host prefetch before `start_operation` (`prefetchMemory`) | `memory_policy.recall`; `preQueryMemory` rewrites the kernel's query. |
| `dependencyOutputsNote` / `dependency_outputs_note` | None. The kernel sends the composed text as `dependency_brief`. |
| `context.dependencyMode: "summary"` | `"full"` with a lower `maxTokens`, or have the upstream node return a short structured output. |
| `context.include` (`WorkflowContextInclude`) | `context.dependencyMode: "none"` to leave dependency outputs out. `memory` and `knowledge` were never read. |
| `loopInstruction`, `classifyInstruction`, `judgeGoal`, `extractClassifyBranch`, `extractJudgeWinner` | None. |
