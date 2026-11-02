# Memory Candidate Receipt ABI (draft)

Status: Rust core, Rust SDK dispatch and the node, wasm and python bridges are implemented; the M1
policy and M2 candidate sink are wired in node, python, wasm and the Rust SDK (sections 8, 9, 11).
Since 2026-10-30 the runtime policy has only two switches, `retrieval` and `modelWrites`; the two
automatic write paths (session extraction and semantic archive) are deleted, and the model proposes
through `propose_memory` (section 15). Content in sections 8–12 about those two paths is historical
record only. Corresponds to contract M2 (`xiaoman-sdk-extension-contract-v2.md` section 3).
Scope: a new canonical kernel outcome so that a model-initiated memory write can express "pending
review" without being recorded as persisted.
Non-goals: this document does not define the six-dimension taxonomy, the user confirmation flow, or
business validation of sources; those belong to the host.

## 0. Corrected premise (verified 2026-10-26)

This draft originally assumed a "model write" was a memory write the model could issue directly.
The source does not work that way:

- The F15 ruling at `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs:1692` is explicit: the
  kernel has no model-facing memory write tool, and the `memory` tool is read-only.
- The only channel for `RequestMemoryWrite` is a child agent's `parent_requests` (child to parent),
  which `plan_memory_write` (`driver/syscall.rs:383`) turns into a `persist_memory` effect.

This spec keeps the `memory` search tool read-only and adds a capability-gated model entry point,
`propose_memory`. It accepts only `name`, `kind`, `content` and optional `description` and
`evidence_refs`, and strictly rejects host fields such as scope, record id, trust, timestamps,
lease, receipt and attestation; it decodes to the existing `RequestMemoryWrite` and reuses
`plan_memory_write`. It is exposed only when memory is enabled, the binding is writable, a
candidate sink exists and `model_writes = candidate_sink`, so a model proposal can only enter
review and never writes the store directly. The host sends `propose_memory_enabled` only when it is
on, so an older kernel still accepts the configuration when the feature is off. Existing child
requests keep `source = child_request` and the existing v2 candidate IDs; direct model proposals use
the v3 identity with `source = model_proposal`.

This revises the F15 ruling above: the model now has a write entry point, but it can only produce a
candidate for review, never write the store directly. This is a product decision, recorded in the
CHANGELOG.

## 1. State before the change

This section describes the protocol before M2 was implemented; the current protocol is in sections
2 and 7. Before the change the kernel had a single memory write receipt.

- Host receipt: `memory_persist_result`, one of:
  - success: `record_ref` + `digest`.
  - failure: `error`.
- Kernel effect outcome: `memory_persisted`, carrying `receipt { binding_id, record_ref, digest }`.
- Every failure path is `failedEffect(..., retryable=true)` (the `error` branch); a missing
  `record_ref`/`digest` is not retryable.

The problem: a candidate write had no "received but pending review" state. Forcing pending into a
persisted receipt makes the host audit record it as written. Mapping it to a failure makes the
effect replay as retryable and submit duplicates. Both violate contract S03.

## 2. Proposal

### 2.1 Host receipt event

A new event `memory_candidate_result`, alongside `memory_persist_result`, without changing the
latter's semantics.

```text
{
  kind: "memory_candidate_result",
  effect_id: string,
  candidate_id: string,            // derived deterministically by the host from operation/effect/ordinal
  status: "pending" | "accepted" | "rejected",
  receipt_id: string,              // stable identifier of the host receipt, used for replay
  reason_code?: string,            // required when rejected
  record_ref?: string,             // required when accepted
  digest?: string,                 // required when accepted
  error?: string                   // used when the call failed (timeout, sink unreachable); other fields omitted
}
```

Constraints:

- `status = "accepted"` must carry both `record_ref` and `digest`; if either is missing the effect
  fails and is not retryable (same as `memory_persist_result`).
- `status = "pending"` carries no `record_ref`/`digest`. It is not a persisted receipt; although it
  uses the candidate branch of the `memory_persisted` success type, the kernel must not produce
  `MemoryWritten` from it.
- `error` only means the call got no receipt. It may be retried, but the retry must use the same
  `candidate_id`, with the host providing idempotency.

### 2.2 Kernel effect outcome

`EffectSuccess` stays a closed union with exactly one variant per EffectKind. `PersistMemory` still
uses `MemoryPersistedSuccess`, in which `receipt` and `candidate` are both optional and exactly one
must be present:

```text
{ kind: "memory_persisted", receipt: { binding_id, record_ref, digest } }
{ kind: "memory_persisted", candidate: { binding_id, candidate_id, receipt_id } }
```

`pending` uses the candidate branch and makes the effect terminal; it produces
`MemoryCandidatePending`, not `MemoryWritten`, and does not change recall state. `accepted` uses
the existing receipt branch; `rejected` is a non-retryable failure. Both branches present or both
absent is `UnexpectedEffectOutcome`. Old receipt-only JSON stays compatible.

The observation is host audit information and is not injected into the model's context. Candidate
identity stays in the sink and host events; an accepted canonical receipt does not store the
candidate_id.

### 2.3 Idempotency and replay

- `candidate_id` is derived from the canonical operation, effect and candidate ordinal and must not
  be regenerated randomly on restore.
- Same `candidate_id`, same payload: the sink returns the original receipt and adds no candidate; the
  sink may be called again after a lost answer.
- Same `candidate_id`, different payload: rejected (`reason_code = "candidate_conflict"`); the
  original record is not overwritten.
- When the journal replays, the effect outcome must come from the persisted `receipt_id`; the host is
  not called again.

## 3. Where the change is implemented

Rust core defines the mutually exclusive outcome in `wire/effect.rs`, settles candidates in
`wire/driver/effects.rs`, and provides the pending observation in `observation.rs`. The canonical
runner bridges of the four SDKs parse `memory_candidate_result`. The node, python and wasm session
logs and kernel event adapters share the 52-kind vocabulary. The runtime policy, sink and write
paths are in sections 8 and 9. The cross-SDK outcome golden fixture is not unified yet; the new
enums and replay counters in the old draft are not considered implemented.

## 4. Compatibility

- The addition is additive: an old kernel does not know `memory_candidate_result`. The host must
  decide whether to send it through a capability version check, and when the capability is not
  detected it must refuse to enable candidate writes rather than fall back to
  `memory_persist_result`.
- The semantics of `memory_persist_result` are unchanged.

## 5. Acceptance

Mapping to section 7 of the contract:

- S03: a model write returns `pending`; both the observation and the event express pending review,
  and it does not count toward recall.
- S04: the sink stored the candidate but the receipt was lost; replaying the same `candidate_id`
  returns the original `receipt_id`, with no duplicate candidate.
- S05: a forged source is rejected by the host with an explicit `reason_code`.

Each state needs one golden test in Rust and in each of the three bridges.

## 6. Questions for the kernel owner

1. Should `pending` make the effect terminal? This draft assumes it is terminal and not retryable;
   if the effect had to wait for review before ending, it would need suspend semantics, which is a
   separate ABI design.

   Research conclusion: it should be terminal. Suspend semantics conflict with four existing kernel
   rules, while a terminal outcome matches the existing shape of memory writes.

   - DEC-3 (`transaction.rs`, "at most one pending effect per kind"): a PersistMemory suspended
     until human review ends would hold the only slot, and every later write in the run would hit
     `ResourceLimitExceeded`. That is a kernel fault the host must rebuild from the journal, not an
     ordinary refusal. Pinned by
     `a_pending_candidate_frees_the_memory_write_slot_and_an_unsettled_write_holds_it`.
   - §5k (`driver/effects.rs`, resuming after memory queries and payload read-backs): as long as a
     sibling effect in the same turn is unsettled, the turn stays in `AwaitingResume`. A suspended
     write would make a query in the same turn wait until review ends.
   - A terminal clears pending effects (`transaction.rs`, "A terminal ends the operation; nothing
     is left waiting on the host"): human review usually finishes after the run ends, so a suspended
     effect would already be cleared before the review result arrives, leaving it nowhere to land.
   - Matching shape: settling an approval (RequestApproval) calls `continue_after` and the loop's
     direction depends on the result, so suspending makes sense there; settling a memory write only
     returns `quiet_step`, nothing downstream depends on it, and the write request itself is
     `needs_workflow_round: false`.

   The cost of terminal: this run's audit stream records only "candidate pending review", not the
   later acceptance or rejection. The candidate store is the only record of the final outcome; an
   accepted record shows up in later recalls. If the model must learn the review outcome within the
   same run, the host should deliver it through the existing signal channel rather than hold the
   effect open.

   Side finding: the only channel for write requests is a child agent's `parent_requests`, and the
   child that requested the write has already finished when the effect settles, so
   `MemoryCandidatePending` is only a host-facing observation that no model context ever receives
   (the same holds for `MemoryWritten`). Section 2.2's "the observation given to the model must
   distinguish three states" therefore has nowhere to land on the current channel, and S03's
   "observation" can only live in the host's session events and audit stream. In addition, §7.12
   says the child completion that terminates the run may not publish any effect: if the last node's
   completion carries a write request, the kernel rejects the whole completion with
   `InvalidLifecycle` (observed while writing this test). This is unrelated to pending semantics, but
   a host implementing candidate writes needs to know it.
2. Does the new event join the existing journal `Mm` category, or need its own category?
3. Naming of the counters in `replay.rs`, and whether `pending` needs its own counter.

## 7. Implementation status (landed parts)

Following the draft above, the following is done:

- Rust core: `MemoryPersistedSuccess` takes exactly one of `receipt` and the new `candidate`; both
  missing or both present is `UnexpectedEffectOutcome`. `pending` is a terminal effect outcome,
  writes no `MemoryWritten`, and produces the observation `MemoryCandidatePending`. Re-delivering the
  same candidate is a replay (DEC-1); a different candidate is a conflict. The wire shape is pinned
  by `a_memory_candidate_outcome_decodes_from_the_bridge_wire_shape`.
- node bridge: `memory_candidate_result`; `pending` maps to the candidate outcome, `accepted` follows
  the existing persisted path, `rejected` is a non-retryable failure, and a transport error is a
  retryable failure.
- Rust SDK `canonical_runner_runtime.rs`: a `memory_candidate_result` branch with the same semantics
  as node, carried by the pure function `memory_candidate_resolution`; five unit tests pin pending,
  accepted, pending with missing fields, rejected and transport error.
- wasm bridge (`wasm/src/runtime/canonical-kernel-step.ts`) and python bridge
  (`python/deepstrike/runtime/canonical_kernel_step.py`): line-for-line the same semantics as node.
  The tests `wasm/tests/memory-candidate-bridge.test.ts` and
  `python/tests/test_memory_candidate_bridge.py` correspond one to one with node's five cases.

The four bridge test layers now all read `tests/fixtures/abi/memory_candidate_receipts.json` (61
cases), each case with a single expected outcome and no SDK special cases. The bridge tests are
reused directly; no new sdk-conformance domain is needed. Real kernel settlement is covered by
separate tests; a bridge test that intercepts the commit is not kernel integration verification.

Handling of the open questions:

1. `pending` is implemented as terminal. The research in section 6 question 1 gives four reasons;
   sign-off by the kernel owner is still pending.
2. The session event vocabulary adds the first-class events `memory_candidate_pending` and
   `memory_candidate_rejected`, updated in node, wasm, python and the shared fixture
   `session-event-vocabulary.json` (52 kinds). The kernel observation `memory_candidate_pending`
   maps in all three SDKs to the session event of the same name (`source = child_request`, no
   `receipt_id`), the same shape as the event the host path writes. Rust's core `SessionEvent` adds
   the two variants of the same names (see section 11), but Rust's session vocabulary is smaller
   than 52 kinds and is not yet in the shared vocabulary consistency check.
3. There is no separate counter for pending.

## 8. Candidate sink (node; the python/wasm port is in section 9)

> **Historical implementation (as of 2026-10-29).** This section keeps the original design and
> implementation record of the M1/M2 candidate sink. `sessionExtraction`, `semanticArchive` and the
> v3 candidate identity of session extraction were deleted on 2026-10-30; the current runtime
> contract is section 15.

Facts verified before implementing: no SDK ever produced a PersistMemory effect. After F15 the model
had no write entry point, and none of the four SDKs filled `request_memory_write` when building
`child_completed`. Every memory write in a real run went through the host funnel `writeMemory`
(`sessionExtraction` session extraction, `semanticArchive` semantic archive, explicit host
`remember`): the kernel only admitted them through `admit_memory_write` and took no part in
settlement. The sink is therefore attached in two places sharing one interface:

- Host funnel: when `memoryRuntimePolicy`'s `sessionExtraction` or `semanticArchive` is
  `"candidate_sink"`, admitted writes go to the sink instead of the store. Session events follow the
  receipt as `memory_candidate_pending` or `memory_candidate_rejected`; only `accepted` is recorded
  as `memory_written` (with `record_id` set to the sink's `record_ref`).
- PersistMemory effect: when `modelWrites` is `"candidate_sink"`, the receipt is submitted to the
  kernel as `memory_candidate_result`. This path has no producer yet; it is covered by unit tests
  and kept for a future producer.

Conventions:

- `RuntimeOptions.memoryCandidateSink`; if any path is `"candidate_sink"` without a sink,
  construction fails. `resolveMemoryRuntimePolicy` no longer rejects `"candidate_sink"` and adds
  three `*ToCandidateSink` booleans. With no policy set, everything goes to the store.
- `receipt_id` is generated by the sink. `candidate_id` is derived from the write's canonical
  identity (`memoryCandidateId`, sha256 of the JSON array of its parts). PersistMemory still uses
  `v2, agent_id, child_request, operation_id, effect_id, 0`; semantic archive still uses
  `v2, agent_id, semantic_archive, session_id, operation_id, effect_id`. Session extraction uses
  `v3, agent_id, session_extraction, session_id, operation_id, payload_digest`, where
  `payload_digest` is the digest of the candidate content and excludes `record_id`, timestamps and
  recall counts.
- Sink contract (`MemoryCandidateSink` docs): same id and same payload return the original receipt
  and add nothing; same id with a different payload is rejected as `candidate_conflict` without
  overwriting; a throw means no answer was obtained and may be retried with the same id. Payload
  comparison excludes storage ids and timestamps. `InMemoryMemoryCandidateSink` is an in-process
  implementation that records every candidate as `pending`.

The v3 identity of session extraction settles the idempotency boundary when the helper model
retries: retrying the same proposal reuses the candidate ID and gets the original receipt; a
proposal whose content changed gets a new candidate ID instead of being mistaken for the same
ordinal candidate; duplicate proposals within one answer are deduped by the runner. Whether
different content is ultimately judged a conflict by a business sink still depends on the candidate
ID and payload the business side chooses.

Acceptance coverage: S03 is covered by end-to-end tests of both real paths, session extraction and
semantic archive (zero store writes, only pending events, no `memory_written`); S04 is covered by
the PersistMemory path (the sink stored the candidate but the answer was lost; re-delivering the
same effect returns the original `receipt_id`, and the sink holds a single candidate). S05 (forged
source) needs a real sink decision, which the in-process implementation does not make.

Candidate archives use candidate result auditing; Node/Python no longer write a candidate's
proposed record_id into `semantic_archive_completed`.

Not done:

- ~~A PersistMemory producer: requires overturning or revising the F15 ruling, a product
  decision.~~ Solved by `propose_memory` (sections 0 and 15).
- S05 and integration with a real sink. Source attestation is implemented in section 14; the
  production sink is still provided by the host.

## 9. Python / WASM port of M1 + M2 (2026-10-26)

> **Historical implementation (as of 2026-10-29).** The four-switch policy, session extraction and
> semantic archive candidate paths in this section are deleted. The current Python/WASM policy
> fields and enabling conditions are in section 15; this section keeps the record of the cross-SDK
> port and compatibility.

Python exports `MemoryRuntimePolicy`, `MemoryCandidateSink`, `InMemoryMemoryCandidateSink` and the
identity functions. The policy accepts a dataclass or a complete dict with the fields `retrieval`,
`session_extraction`, `model_writes` and `semantic_archive`; RuntimeOptions uses
`memory_runtime_policy` and `memory_candidate_sink`. WASM uses the same camelCase configuration and
types as Node, exported from the package entry and the memory entry. Without a policy the old
behavior stays; with a policy all four switches must be present and valid, and a candidate path
without a sink fails construction.

Both check the switches before the prefetch hook, the session extraction model and the archive
summarizer/fallback model. The read/write of the canonical memory binding project
retrieval/modelWrites respectively. The host funnel and PersistMemory execution share the sink;
accepted must carry record_ref/digest, rejected must carry reason_code, and pending does not call
store.put or produce memory_written. Explicit host writes keep their interface and direct-write
behavior.

Candidate identity uses the JSON array rule of section 8. WASM uses a browser-friendly synchronous
SHA256 and TextEncoder, with no node:crypto/Buffer; fixed hash vectors confirm agreement with
Node/Python and that a digest has exactly one sha256 prefix. Both use the captured session/effect
IDs for archive identity instead of timestamps. WASM waits for the archive commit to finish so no
task keeps reading mutable session state after the run ends; the cost is that helper model/sink
time is added to run latency.

The port also fixed the loss of action/tier in compression observations during canonical projection
adaptation, without which semantic archive might never fire. On the candidate path Python uses only
candidate result auditing and does not record the proposed record_id as semantic_archive_completed;
Node was aligned in the section 10 convergence. WASM uses candidate audit events and adds no archive
completion event.

Verification: Python built a real Linux native extension and ran the tests loading it separately,
without overwriting the existing Mach-O extension; 114 related tests passed during implementation,
95 in the integration review, and 31 candidate/archive tests after the archive audit fix. Coverage
includes native child request permissions, pending/accepted/rejected settlement, and the real
session extraction and real semantic compression paths. The full Python suite was not run. WASM
TypeScript checks and build pass; the full Jest run (49 suites, 229 tests) passes, and 12 runner
tests pass after adding accepted/malformed receipt and disabled modelWrites tests. WASM Jest uses a
mock kernel; archives are covered through helper and projection passing tests, and the real WASM
binding integration tests were not run this time.

The boundaries stay explicit: the in-process sink does not persist across processes and does not
perform S05 source attestation; the SDKs have no regular PersistMemory producer yet. The candidate
identity for extraction retries is now v3 payload-addressed: the same proposal replays stably, and
different content no longer conflicts wrongly by reusing an ordinal. Idempotency across process
restarts and concurrent persistence still needs verification with a production sink. Candidate
identity and payload comparison are extended in this section and section 10.

## 10. Cross-SDK contract convergence (2026-10-26)

> **Historical implementation record.** This section records the identity and payload rules adopted
> when the candidate paths of sections 8 and 9 converged across the four SDKs. After the automatic
> extraction paths were deleted they still apply to the remaining `PersistMemory` candidate sink,
> but they no longer imply that `session_extraction` or `semantic_archive` are available policy
> fields.

Node/Python/WASM runtime identities use the v2 array rule of section 8. A new run uses a new
operation_id even if it reuses the session_id; resuming the same run keeps its operation_id.
Archives use the original effect_id, not the name-sanitized record_id, as the idempotency identity.
The in-process sink isolates by `(agent_id, candidate_id)`, so different agents using the same custom
ID do not share receipts; a change of source under the same agent and ID is a conflict. Input
candidates, returned receipts and query results are deep copies, so callers cannot mutate state the
sink holds.

The review payload includes scope, kind, name, description, content, provenance
author/trust/session_id/evidence_refs, plus confidence, links, pinned and ttl_days; source is
compared separately. Only the storage record_id, timestamps and recall counts are excluded from
conflict comparison. Array order matters. agent_id is a host-provided namespace and gives no
authentication or anti-forgery guarantee about the source.

Migration note: v2 changes candidate ID derivation and the payload digest. An existing custom
persistent sink must not switch identity versions while old runs may still re-deliver; let the old
runs drain under the original SDK/identity rules, or have the host keep an explicit mapping from old
IDs to the new identity. The sink this repository ships is in-process only and performs no
persistence migration.

The shared receipt fixture has 61 cases; all four SDKs require non-empty strings for the required
pending/accepted fields, and numbers, booleans, arrays and the like are never implicitly converted
into receipt IDs. The accepted bridge keeps its old compatibility and does not newly require
candidate_id/receipt_id. A non-null error takes precedence, including the empty string, 0 and false;
a string is used directly as the error message, a structured value as compact JSON (object keys
sorted), and the result is retryable. A rejection reason that is not a non-empty string becomes
unspecified. Each case has a single canonical outcome, and candidate/receipt stay mutually
exclusive.

The real WASM test `wasm/tests/memory-candidate-binding.node.mjs` is part of
`test:canonical-binding`. After rebuilding the binding from current source, the test actually
publishes PersistMemory; settles pending/accepted/rejected through runner/sink/bridge; and covers
disabled write permission, re-delivery after a lost receipt, DEC-1 same-outcome replay,
different-outcome rejection, journal restore and two extractions in one session. The local pkg-node
binary had previously still been the old receipt-only protocol, and a real commit reported unknown
field candidate; after rebuilding it passes, which shows a test that loads the binding but
intercepts the commit is not enough to prove ABI compatibility.

This round's verification: Node build and the full Jest run, 207 suites / 1399 tests, pass (6 suites
/ 14 tests skipped); WASM build and the full Jest run, 49 suites / 291 tests, pass; the 5 candidate
integration scenarios on the rebuilt binding, the canonical binding check and 74 golden checks pass;
139 related regressions pass against a real Linux Python extension, without running the full suite;
the shared receipt test passes in the full Rust SDK test target, walking all 61 cases, without
running the complete Rust test set this round. Idempotency across process restarts and concurrent
persistence awaits verification once a production sink is implemented.

## 11. Rust SDK port of M1 + M2 (2026-10-27)

> **Historical implementation record.** The four-switch policy and the automatic extraction/semantic
> archive paths in this section are deleted; the remaining Rust candidate sink, PersistMemory receipt
> and identity functions follow the current sources described in section 15.

`rust/src/memory.rs` exports `MemoryRuntimePolicy` (`retrieval`, `session_extraction`,
`model_writes`, `semantic_archive`; serde snake_case, all four switches required, unknown fields
rejected), the `MemoryCandidateSink` trait, `MemoryCandidate`/`MemoryCandidateReceipt` (tagged by
`status`; the JSON is the body of `memory_candidate_result`), `InMemoryMemoryCandidateSink`, and
`memory_candidate_id`/`memory_candidate_payload_digest`. Identity and payload digest follow the rules
of sections 8 and 10; the fixed vector `candidate:ead40e56…7043` matches the other SDKs, and an
integer confidence serializes the way JSON.stringify writes it.

`RuntimeOptions` adds `memory_runtime_policy` and
`memory_candidate_sink: Option<Arc<dyn MemoryCandidateSink>>`. Because `RuntimeRunner::new` does not
return a `Result`, the missing-sink check sits at the `run_streaming*` entry points and fails before
the run starts, without calling any model. With no policy set the old behavior stays.

Integration points: `set_memory_enabled` carries the read/write bits, fixing the Rust bridge, which
previously ignored them and always granted read/write; with retrieval off, the prefetch hook is
skipped, model QueryMemory returns empty results and host `query_memory` returns empty; session
extraction and semantic archive check the switches before calling the helper model, and the
candidate path goes to the sink through the private funnel `write_memory_routed`; PersistMemory
settles with `memory_candidate_result` when `model_writes = candidate_sink`, and a sink error is a
retryable `error`. The public `write_memory` signature and direct-write behavior are unchanged.

The explicit `remember()` of all four SDKs also goes through the same memory write funnel: kernel
admission first, then the store or the candidate sink according to the runtime policy; on rejection,
on no bound store, or when the sink rejects, the facade reports an error instead of returning a
record that was never written. Explicit `recall()` goes through the unified retrieval gate, the
`retrievalTopK` limit, the kernel-derived recall lifecycle and `memory_retrieval_result` auditing;
it returns empty results when retrieval is disabled.

Fixed along the way: semantic archive names are derived from the page-out effect instead of a
timestamp (the same rule as Node's `stableSemanticArchiveName`), `provenance.session_id` uses this
run's session_id, and audits are written to this run's session. Core `SessionEvent` adds
`MemoryCandidatePending` (`receipt_id` optional) and `MemoryCandidateRejected` in the Mm category;
the kernel observation `MemoryCandidatePending` projects to the session event of the same name with
`source = child_request` and no `receipt_id`. Rust does not emit
`semantic_archive_pending/completed/failed`, as before.

Verification (Linux sandbox, rustc 1.99.1): all 162 `deepstrike-sdk` lib tests pass, 15 of them new
(4 in memory.rs; 11 in `runtime/runner_memory_candidate_tests.rs`, covering refusal to start without
a sink, S03 session extraction end to end, direct writes with no policy, zero model calls/zero store
access with everything off, S04 re-delivery returning the original receipt and conflict rejection,
retryable sink errors, a single candidate for the same archive effect, zero calls with archive off,
kernel observation projection and rejection auditing); all 1169 `deepstrike-core` lib tests pass;
187 `deepstrike-tests` pass with 23 ignored; `cargo check` passes for the wasm/node/py binding
crates. clippy/rustfmt were not run (the minimal toolchain lacks them).

## 12. Context projection and facade convergence (2026-10-28)

M4/M5 converge the explicit memory API and runtime prefetch onto one kernel-owned rule chain:

- `remember()` goes through kernel admission first, then the direct store or the candidate sink per
  `memoryRuntimePolicy`; `false` is no longer swallowed by the facade but reported as a write
  rejection.
- `recall()` and `pre_query_memory` are both bound by the retrieval gate and `retrievalTopK`, and go
  through the same recall lifecycle; a hit updates the recall count, triggers the promotion edge and
  writes `memory_retrieval_result`.
- The initial prefetch adds each hit as an ordinary history user turn; the canonical text is
  `[MEMORY record_id=<id> kind=<kind> trust=<trust>] <content>`.
- During renewal a running canonical operation does not accept history appended directly by the
  host, so re-query hits enter the new sprint as keyed knowledge `memory:<record_id>`. The entry uses
  the same canonical text and is deduped by record ID.
- Node, Python, WASM and Rust all export the shared recall projection/helper, verified by fixed
  vectors and cross-SDK conformance.

This round's verification includes the four-SDK shared-evidence snapshot/conformance, the M4/M5
regressions in Node/Python/WASM/Rust, the P0/P1/P2 boundary checks on the real WASM kernel, the
candidate binding and renewal re-query. The WASM `test:canonical-binding` entry needs `wasm-pack`
installed to rebuild `pkg-node`; the environment lacked it, so this round ran all real boundary
scripts on the existing binding and passed the renewal binding after running `npm run build` first.
The Node payload-store age-cleanup case is still affected by sandbox file time/deletion permissions;
S05 real source attestation and a cross-process production sink are still not implemented.

## 13. Durable candidate ledger (2026-10-28)

Node, Python and Rust add `FileMemoryCandidateSink`, a file-based pre-production reference
implementation; WASM still accepts only a host-injected `MemoryCandidateSink`. The three share these
semantics:

- The snapshot is keyed uniquely by `(agent_id, candidate_id)` and stores the candidate payload
  digest, source, status receipt, record reference, record digest and timestamps.
- The same key with the same payload/source returns the stored original receipt; the same key with
  a changed payload or source returns a new `candidate_conflict` receipt without overwriting the
  original candidate.
- `resolve()` allows exactly one terminal transition from pending to accepted/rejected; resolving
  again returns the original terminal receipt. Accepted `record_ref`/`digest` and rejected
  `reason_code` are persisted with the receipt.
- Candidate submission, acceptance, rejection and outbox acks are published atomically with the
  ledger snapshot. File writes use a temporary file, fsync and rename; a lock directory serializes
  cross-process writers. On JSON or schema corruption, recovery fails closed instead of rebuilding an
  empty ledger.
- `outbox()` returns host events not yet acked; `ackOutbox()` idempotently removes the given event. A
  database or message queue implementation can replace this reference sink without changing the
  `MemoryCandidateSink.submit()` ABI.

## 14. Source attestation/verifier (2026-10-28)

Source attestation is separate from candidate business identity. Candidate identity is still
determined only by `(agent_id, candidate_id, payload_digest, source)`; the attestation does not enter
the business payload digest. Attestation attempts are recorded separately with
`attestation_digest`, issuer, key_id, reason code and time; a failure does not create a formal
candidate ledger entry and does not occupy the candidate identity. A rejected receipt under the
compatible `submit()` ABI only means this submission was not admitted, and a retry with a new
attestation may use the same candidate ID.

The attestation canonical JSON array contains `issuer`, `key_id`, `agent_id`, `tenant_id`,
`namespace`, `candidate_id`, `source`, `payload_digest`, `issued_at`, `expires_at` and
`policy_version`. The verifier receives the agent boundary through `verify(agent_id, candidate)`; by
default it allows 60 seconds of clock skew and a maximum validity of 10 minutes. HMAC-SHA256 stays as
the portable reference verifier; Node, Python and Rust also provide an Ed25519 verifier that holds
only the public key, with the signature format `ed25519:<base64url>`, and both verifiers use the same
canonical payload.

A shared admission helper runs before every in-memory/file sink: `host_verified` must be proven by a
`host` author, a valid attestation, matching agent/scope/source/digest, a trusted key and a valid
time window together; model/extraction/child candidates may still enter pending as untrusted or
user_asserted, but cannot promote themselves to host_verified by editing provenance.

This round adds basic cross-SDK memory lifecycle rules: a lease is bound to
`agent_id + tenant_id + namespace` and rejects writes by `expires_at`, revocation time and
`max_writes`; records past `ttl_days` are not returned by search of the reference in-memory store;
the host can run deletion sync with a tenant/namespace scope predicate. Lease state in the durable
file sink and cross-filesystem transactions are still later integration work. WASM contains no file
API, to keep the Node/Python/Rust host file assumptions out of the browser package.

## 15. Model proposal loop and removal of automatic writes (2026-10-30)

This section is the current state and supersedes the descriptions of session extraction and
semantic archive in sections 8–12.

The runtime policy has two switches: `retrieval` (`"disabled" | "host"`) and `modelWrites`
(`"disabled" | "candidate_sink"`). The host funnels `sessionExtraction` and `semanticArchive` and the
v3 candidate identity of session extraction are deleted; unknown fields are rejected as validation
errors in all four SDKs. Memory writes therefore come from three sources only: the model's
`propose_memory` (`source = model_proposal`), a child agent's `request_memory_write`
(`source = child_request`), and the host's explicit `remember()`. The first two both become
PersistMemory, the one place the sink is attached.

Calling and settling `propose_memory`:

- On the call the model immediately gets the tool receipt "memory proposal submitted for review…",
  and the turn waits for the PersistMemory to settle.
- After the host settles it, the kernel writes `[MEMORY PROPOSAL name=<name>] <outcome>` into
  history as a user turn and resumes the turn. The outcome is `stored`,
  `pending review as candidate <id>; it is not stored until the host accepts it.` or
  `not stored: <reason>`. When sibling effects in the same batch are still unsettled, the last one to
  settle resumes the turn.
- The author of a child request has already finished, so settlement records only an audit
  observation and writes no history. The two are told apart by `AuthoredMemoryWrite.model_facing`,
  which is persisted in the checkpoint.

DEC-3 allows only one unsettled PersistMemory. When a model proposal runs into it, the turn is no
longer aborted with `ResourceLimitExceeded`:

- Identical binding, `name`, `kind` and `content` to the pending proposal: the original effect is
  reused and the receipt says "already pending review".
- Different content: returned as an ordinary tool refusal, "Another memory proposal is already
  pending review. Propose this one again after its outcome arrives.", consuming no quota.
- A child request hitting DEC-3 is still a fault.

There is no batch submission; one at a time is enforced by the tool description (the `propose_memory`
description in `context/manager.rs`, which also says what to record and what not to, and to search
with `memory` first and reuse the name).

Not done:

- ~~`model_proposal` and `child_request` use separate candidate ID schemes, and the sink does not
  dedupe across sources by payload digest.~~ Done on 2026-10-31, see section 18.
- Recall placement still has two forms: the first-turn prefetch goes into history, the renewal
  re-query into keyed knowledge `memory:<record_id>` (section 12). This is deliberate: a running
  canonical operation can write history only through an effect or an external event.

## 16. Recall becomes index push + on-demand pull (2026-10-30)

Host prefetch (first-turn history and renewal keyed knowledge) no longer injects record bodies; each
hit pushes one index line: `[MEMORY record_id=<id> kind=<kind> trust=<trust>] <summary>`, where the
summary is the `description`, or the `name` when that is empty. The four SDKs share two fixed
vectors: record-v with a description gives `… how briefs are laid out`, and with a blank
description gives `… fact`.

The model reads the full record with the new `record_id` parameter of the `memory` tool:

- The tool schema no longer requires `query`; exactly one of `query` and `record_id` must be given,
  otherwise it is returned as an ordinary tool refusal without aborting the turn.
- `CanonicalMemoryQuery` adds an optional `record_id` (`MemoryRecordRef`), with `requested_k` fixed at
  1 and empty `text`. The checkpoint's `AuthoredMemoryQueryState` adds the optional `record_id` too,
  and old checkpoints still restore. DEC-3's reuse check includes `record_id`.
- The host does a point lookup with `MemoryStore.get` and filters it through the shared helper
  (Node/WASM `memoryFetchRecall`, Python `memory_fetch_recall`, Rust `memory_fetch_recall`): a record
  that does not exist, is out of scope, is not in the requested kinds or is past its TTL is returned
  as not found (empty); the same retrieval switch as search applies.
- The kernel accepts only exactly the requested record or no result; any other record or multiple
  results are rejected as `UnexpectedEffectOutcome` with no state change, and the effect stays
  pending. A hit is written into history as `[MEMORY record_ref=<id> kind=<kind>] <content>`, the
  same as a search hit, and the recall lifecycle is derived as usual; with no result,
  `[MEMORY record_ref=<id>] not found: …` is written so the model knows the index line is stale.
- The `query` field of the `memory_queried` observation records a point lookup as `record_id=<id>`;
  no event field is added.

A model-initiated `memory(query)` search still returns full text; only host pushes are indexed.

## 17. Candidate identity and source checks move into the kernel (2026-10-31)

Candidate IDs, payload digests, the attestation canonical payload, HMAC signatures and every
attestation check except the Ed25519 signature are now implemented once, in Rust core
(`mm::memory_candidate`). The Rust SDK re-exports them directly; Node, Python and WASM call them
through the existing `memoryAuthorityJson` bridge, with no new binding functions.

Four new operations:

| op | Request | Answer |
|---|---|---|
| `candidate_id` | `parts: string[]` | `{ candidate_id }` |
| `candidate_payload_digest` | `record` (the SDK's record JSON; unknown fields ignored) | `{ payload_digest }` |
| `attestation` | `attestation`, optional `secret` | `{ canonical_payload, digest, signature? }` (an HMAC signature when `secret` is given) |
| `verify_source` | `agent_id`, `candidate`, optional `verifier { trusted_issuer?, hmac_secret?, window }` | `{ valid, reason_code?, signed_payload? }` |

`verify_source` applies the source rules first: only host may declare `host_verified`, and it must
carry an attestation; any candidate that carries an attestation is verified. Without a `verifier`, a
candidate that needs verification gets `attestation_verifier_unconfigured`, and the SDK hands it to
the host verifier. With a `verifier`, checks run in a fixed order: untrusted issuer →
agent/tenant/namespace mismatch → candidate mismatch → source mismatch → digest mismatch →
policy_version mismatch (reported as `signature_invalid`) → inverted or overlong validity (reported
as `expired`) → not yet valid → expired. When `hmac_secret` is given the kernel also checks the
signature; otherwise a passing answer carries `signed_payload`, whose bytes the host verifies with
the Ed25519 public key. Ed25519 keys never enter the kernel.

The canonical form keeps the compact `JSON.stringify` array the TypeScript SDK has always used, so
existing IDs and digests are unchanged (the four fixed vectors still pass). Numbers are written per
ECMAScript `Number#toString`, which fixes Python's form for decimals (previously `1e-06`, now
`0.000001`).

The convergence also unified three divergences: an issuer/key mismatch used to be `scope_mismatch`
in Rust/Python and is now `issuer_untrusted` everywhere; an inverted validity used to be
`signature_invalid` in the Rust/Python HMAC verifier and is now `expired` everywhere; a
`host_verified` without attestation used to look for a verifier first in Rust and now reports
`attestation_missing` directly.

The WASM kernel loads asynchronously, so the WASM identity and verification functions now return
Promises, and `MemoryCandidateSourceVerifier.verify` may return a Promise. The WASM jest mock
implements the same rules for tests only; byte equality with the real kernel is pinned by
`memory-candidate-binding.node.mjs`.

## 18. Sink dedupe by content (2026-10-31)

`model_proposal` and `child_request` derive candidate IDs separately, so the same fact proposed
through both paths, or through different effects of one path, gets different IDs. After the
idempotency check (same ID) and source verification, and before storing, the sink now checks whether
the same agent already has a candidate with the same payload digest:

- The existing one is still under review (pending): rejected with
  `reason_code = "already_pending_review"`.
- The existing one was accepted: rejected with `reason_code = "already_remembered"`.
- The existing one was rejected: not a duplicate, and may be proposed again. The host may have
  changed its mind.

A dedupe rejection does not occupy the candidate ID and writes no ledger entry; a durable sink still
records a `candidate_rejected` outbox event. The model sees
`not stored: memory candidate rejected: already_pending_review`. All four SDKs export the same
decision function (Node/WASM `memoryCandidateDuplicateReason`, Python/Rust
`memory_candidate_duplicate_reason`), which a host implementing its own sink can use directly.

## 19. Write governance moves out of the runner (2026-10-31)

Host-side write governance used to be scattered across the four runners: Node and Python charged the
lease budget in the runner, and all four validated sink receipts and computed the digest of stored
receipts there. Each SDK's memory module now has a `MemoryWriter`:

- `persist(agentId, record, writeId)`: with a lease store, `consumeWrite` first (`writeId` makes a
  re-delivery charge only once), then write the store and return `{ record_ref, digest }`. WASM and
  Rust have no lease store, and their signature has no `writeId`.
- `propose(agentId, candidate)`: hand it to the sink and validate the receipt per §2.3 (it must name
  this candidate and carry `receipt_id`; accepted carries `record_ref`/`digest`; rejected carries
  `reason_code`).

The runner only chooses the route (store or sink) and records session events; it no longer touches
the lease or the receipt contract directly. Behavior is unchanged and the existing tests of all four
SDKs are unchanged; Node and Python also have unit tests for `MemoryWriter`.
