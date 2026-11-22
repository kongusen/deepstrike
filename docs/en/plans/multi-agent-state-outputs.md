# DeepStrike Multi-Agent Collaboration and Context Versioning (Deferred)

> **Status: deferred, not to be implemented.** Evaluated 2026-11-22 against `main` (v0.2.85).
> Sections 1–7 below are the original 2026-11-21 draft, kept unchanged.
>
> **Why it is deferred**
>
> 1. **No caller needs it.** The existing verification paths all pass values and run in order, so
>    a candidate cannot be replaced while it is being verified:
>    - CreatorVerifier / OrchestrationMode
>    - milestone evaluation
>    - verify nodes in a static DAG
>    - Harness retries
>
>    Since 0.2.85, `ctx.parallel` in a dynamic workflow really runs concurrently. But a dynamic
>    workflow's host script decides when the workflow is done. It holds the value each
>    `ctx.agent()` returned, so it can check directly whether a verification was of the current
>    candidate. No kernel adjudication is needed.
> 2. **It conflicts with [DEC-13](../decisions/013-model-led-context.md).**
>    - P2 has the kernel parse the model's final reply as a strict JSON verification report, and
>      write the verification subject into the child agent's brief. That is a semantic layer in
>      the kernel.
>    - The "verification evidence" is the verifier agent's own `passed: true`. Binding identities
>      stops evidence about an old candidate from completing a new one. It does not make the
>      verification itself more trustworthy.
>    - The pilot metrics in section 7 (rejection counts, redelivery hits) are not the paired
>      measurement DEC-13 §3 requires.
> 3. **Most of P1 already holds.**
>    - Dependency outputs and inherited knowledge are written into the launch metadata at dispatch.
>    - Harness retries reuse the same built child-run input.
>    - Only one input still changes after dispatch: a full-inheritance child agent reads the parent
>      SessionLog when it is built, so later batches see earlier siblings'
>      `workflow_node_completed` events. That is context changing over time, not a stale
>      verification being accepted.
>
> **Done separately**
>
> - P0, the lease race: #12, v0.2.85.
> - The dynamic workflow's open flag being lost on checkpoint restore, which the draft also
>   touched: #13, v0.2.85, fixed for every dynamic workflow.
>
> **The partial implementation.** It lived on local commit `b3912d47`, was never pushed, and will
> not be merged. Its problems:
>
> - A workflow that never gets a verified candidate cannot close.
> - Every verifier node must be declared at start.
> - A malformed report is dropped silently.
> - There is no end-to-end test on the Node side.
>
> If this work restarts, re-implement it in the stages of section 7 rather than reviving that code.
>
> **When to re-evaluate**: when either of these happens:
>
> - a real workflow needs to replace a candidate while an older one is being verified, with the
>   kernel, not the host, deciding completion;
> - a measurement shows full inheritance reading sibling outputs hurts child agents' results.
>
> Before re-evaluating, design a paired measurement per DEC-13 §3.

Design draft · 2026-11-21 · code baseline 0.2.84 (de1974e7)

## 1. Goals and first-phase scope

Two agents can read the same context at the same time and each propose a fix or a verification.
Before the parent workflow accepts "task complete", it must be able to answer three questions:
Which artifact was verified? Is it still the current candidate? Has this right to complete already
been used? The first phase delivers one complete scenario, "a candidate artifact passes
verification", using the existing DAG, child-task attempts and canonical Journal.

The design adds two capabilities: pin the input version at dispatch, and atomically consume one
exclusive state output at submission. Evidence and candidate artifacts can be referenced
repeatedly. Each shared object is adjudicated by one operation, and each agent keeps its own run
context. The protocol works within one parent workflow; it needs no consensus nodes and no global
commit chain.

## 2. Current implementation and where it changes

| Paths checked | Behaviour today | Change in this plan |
| --- | --- | --- |
| [Rust dispatch](../../../crates/deepstrike-core/src/runtime/kernel/wire/driver/continuation.rs), [child report](../../../crates/deepstrike-core/src/runtime/kernel/wire/driver/events.rs) | The kernel issues attempt identities and checks them on completion. Dependency text comes from the upstream final_message, addressed by agent ID. | Bind the producing attempt to a specific artifact. Completion checks add a business pre-state condition. |
| [Child run construction](../../../node/src/runtime/sub-agent-orchestrator.ts), [Node Runner](../../../node/src/runtime/runner.ts) | A child run receives inherited knowledge and dependency results. Full mode reads the parent SessionLog when the child run is built. | Pin a read manifest at dispatch, so direct runs and Harness retries use the same fixed input. |
| [Knowledge partitions](../../../crates/deepstrike-core/src/context/partitions.rs), [ItemGraph](../../../crates/deepstrike-core/src/context/graph.rs) | An update to the same key can wait until a context boundary to take effect. The graph prunes relations by capacity. | Record the version of what was actually delivered. Important commit dependencies go explicitly into the Journal; the graph stays a capacity-bounded query view. |
| [Canonical step](../../../node/src/runtime/canonical-kernel-step.ts), [KernelJournal](../../../node/src/runtime/kernel-journal.ts) | After prepare, append at the operation's expectedHead, then apply the kernel commit. Conflicts can be recovered and retried; an acknowledged checkpoint can trigger prefix truncation. | Reuse this commit path: in prepare, decide whether the business input can still be consumed; cover the new state in checkpoints. |
| [Memory candidates](../../../node/src/memory/candidate-sink.ts), [in-memory lease store](../../../node/src/memory/lifecycle-store.ts), [file store](../../../node/src/memory/durable-lifecycle-store.ts) | Candidate dedup, review receipts and write budgets exist. In the in-memory store, concurrent consumption of a single-write budget was reproduced: accepted twice, counted once. | First unify lease concurrency semantics. Later, shared memory publishing reuses candidate review and separately checks the target version. |

## 3. Dispatch protocol: pin the input a child agent receives

At dispatch the parent run builds an input manifest and persists it before starting the child run.
Suggested fields are below; the exact serialization goes into a shared wire fixture.

| Field | What it records and why |
| --- | --- |
| Child-task identity | Parent operation, task, attempt: locates this dispatch. |
| Parent read boundary | The identity of the parent SessionLog read and up to which session seq; also the matching canonical source position. The two logs use their own cursors. If their correspondence cannot be proven, first freeze an immutable copy of the selected events and store a reference. |
| Inherited knowledge items | Source operation, item, digest of the visible content, a readable content reference; key, pin and scope filters as today. For an entry with a pending update, take what was actually delivered at dispatch. |
| Upstream artifacts | For each dependency, the producing operation, task, attempt, content digest and payload reference. The existing dep aliases bind to the pinned artifact within that child run. Two executions that produce identical bytes still keep their own provenance. |
| Required preconditions | The objects to check when state is advanced later, such as the current candidate output, the test baseline or the acceptance rule version. Ordinary reference material can be reused freely. |

The dispatch manifest says "what was delivered". The child run still reads content on demand as
today, and read events say "what was retrieved". Neither proves the model understood any of it.
Context compaction, cache reordering and private note updates keep their current mechanisms; only a
change to a precondition declared by a business rule affects a commit. A version reference must
resolve to the matching bytes within the authorized scope; when the content is past retention, it
should report it missing, not silently read the latest version.

## 4. Commit protocol: who accepts a business transition

In the first phase the parent workflow operation holds the state output for the current candidate.
An output's identity is "owning operation, the transaction ID that created it, output index". Every
replacement creates a new identity, so even if a candidate with the same content is published
again, an old completion request cannot take effect again.

A **fact commit** accepts one sourced statement on its own, for example "a test run verified P1 on
baseline B and passed". The record includes the producing attempt, the tested artifact's digest,
the baseline, the acceptance rule version, the result and an evidence reference. Accepting it
means keeping the result of that verification, scoped by those fields.

A **state transition** submits a stable request ID, the operation type, the output ID it expects to
consume, the required fact references and the rule version. The caller's identity comes from a
verified run or host binding. The kernel produces the successor output by rule. For a completion:

1. Look up the receipt by request ID within the owning operation. An accepted request with the same
   ID and payload returns the original receipt; the same ID with a different payload returns a
   conflict. Bind the payload using canonical bytes and the protocol version.
2. In prepare, check caller authority, the current attempt, that the input is still valid, and
   that the transition type is legal. A completion also requires that the verification fact's
   artifact, baseline and acceptance rule match the current candidate, and that the evidence source
   meets the workflow's requirements.
3. In one canonical record, write the consumed input, the produced successor and the adopted fact
   references. After commit through the existing Journal CAS, the kernel updates the active output
   and receipt projections. The first issue of a candidate is done by an authorized workflow
   operation, which checks that the logical object has no current output.
4. On a CAS conflict, restore the latest state and re-run prepare with the original request and
   input. If only unrelated records were appended, it can still succeed; if the target output has
   been consumed, return a business conflict with the disclosable current reference. A caller that
   wants to re-apply on the new version generates a new request.
5. If the process crashes after persisting, recovery rebuilds the consumption result. External
   side effects such as tests and notifications are coordinated by stable operation IDs and
   receipts; replay only applies recorded results.

Expose three outcomes to SDKs: accepted (including redelivery of the original receipt), pre-state
changed (context can be fetched again), and invalid request (identity, evidence or payload
mismatch). Keep the existing attempt rejection of late completions. A late test report that should
be kept goes in through a separate, authorized evidence entry; it cannot restore an old attempt's
right to complete.

## 5. Acceptance by one concurrent scenario

| Step | Fact or action accepted | State that can advance |
| --- | --- | --- |
| 1 | A submits a diagnosis of the restore difference and a failing sample. B submits patch P1. | The parent issues O1, pointing to P1, test baseline B1 and rule R1. |
| 2 | C receives the pinned P1 input and starts verifying. | O1 is valid. C's report must identify P1/B1/R1. |
| 3 | B submits a replacement with P2; the parent consumes O1 and issues O2. | The current candidate is O2. The diagnosis, sample and P1 can still be read. |
| 4 | C returns a passing report V1 for P1, then applies to complete O1 with V1. | The report is kept. O1 is consumed, so the completion returns a conflict. Completing O2 with V1 is also rejected because the artifact does not match. |
| 5 | A verifier verifies P2 on the target baseline, producing V2. The parent accepts the completion of O2. | O2 is consumed once; the completion output references P2 and V2. Redelivering the same request returns the original receipt. |

When a replacement and a completion arrive together, both compete for the same O1, and the same
parent Journal decides the order. If the completion is accepted first, the replacement must follow
the business rule for reopening after completion. When exclusive claims are added later, each
execution attempt gets its own claim output; a timeout recovery consumes the current claim output
and issues a new one, keeping the claim history.

## 6. Replay, retention and cross-SDK compatibility

- **Version negotiation.** New workflows opt in to versioned commits explicitly. Old Journals and
  old checkpoints restore under the original protocol; missing historical dependency versions are
  marked unknown. Only newly created candidates with full provenance enter the strict verification
  flow. An old SDK that meets a workflow requiring the new capability reports the mismatch before
  starting.
- **What replay restores.** Checkpoints cover active outputs, the fact bindings adjudication needs,
  accepted requests with payload digests and receipts, rule versions, and the input manifests (or
  recoverable references) of unfinished dispatches. Recovery rebuilds from the checkpoint plus the
  tail records; it does not call the model again, re-run tests, or re-judge old facts by today's
  rules.
- **Retention boundary.** Before allowing a Journal prefix to be truncated, verify that the
  checkpoint and its references can be restored. Fact bodies and artifacts needed for long-term
  review are archived separately under the project's retention policy. For requests that promise
  lasting idempotency, the receipt index must cover the retry period the protocol promises.
- **Shared implementation.** The Rust kernel owns the authoritative transition rules. The Node,
  Python, WASM and Rust SDKs carry references and receipts for the entries they support and reuse
  one set of serialization, conflict and recovery samples. When the host Memory store and the
  parent Journal are separate commit domains, publishing advances by idempotent receipts, and a
  stage is reported complete only after it is persisted.

## 7. Implementation order and delivery gates

| Stage | Independently deliverable result | Acceptance before the next stage |
| --- | --- | --- |
| P0 Fix the lease race | In the Node in-memory store, do lookup, verification, counting and receipt registration synchronously. Share the consumption contract test with the file store. | With a single-write budget, exactly one of several concurrent different requests is accepted; concurrent redeliveries of the same request return the same result. Accepted events, budget use and receipts agree. |
| P1 Versioned handoff | Rust dispatch metadata carries artifact identity and the input manifest. Node first wires dependency reads, full inheritance, direct runs and the Harness path. | After dispatch, with a parent-log append, a same-key content replacement, and an upstream retry, a started attempt still reads its pinned input; old-log restore samples pass. |
| P2 Complete after verification | The kernel supports issue, replace and verified completion, wired into the parent Journal's prepare/commit, checkpoints and SDK receipts. This stage forms the first shippable loop. | A P1 report cannot complete P2; of two competing requests only one advances; unrelated objects commit independently; crash after persist, restore after prefix truncation and same-request redelivery all give consistent results. Run the same contract samples on every SDK that enables the capability. |
| P3 Shared Context / Memory publishing | Assign commit ownership to explicitly shared conclusion keys; after a candidate passes review, publish a new version against the expected old output. Record supersede, refute and verify relations between facts. | Different keys update independently; competitors on the same key get a conflict and can propose again; rejected or stale candidates keep a traceable result; existing read permissions, budgets and deletion policies still apply. |

Pilot first on an existing creator/verifier workflow. Observe the number of rejected completion
requests for old artifacts, same-request redelivery hits, verifications actually re-run because of
version changes, and the dispatch manifest's effect on start-up time and context tokens. Establish
a P1 baseline first, then compare P2 on the same kind of workload. Roll out when one real run can
answer: who produced a result from which inputs, which evidence supported the final completion, and
why a competing commit did not take effect.
