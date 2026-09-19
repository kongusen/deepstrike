---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [KernelEffect, KernelInput, PlannedStep, SyscallRequest]
  python: [RuntimeRunner, KernelJournal]
---

# Runtime Causality & the Chain Validator

This page promulgates: ① the global causality chain (10 hops) with each hop's interlinking field; ② the mixed-batch publication constitution M1–M5; ③ the five admission gates; ④ chain-validator rules C1–C8. Principle A4: cross-layer relations link by ID, never by "roughly this order".

> Provenance: P2 Identity & Causality and P5 Intent/Effect/Outcome (2026-09-15, archived in `.local-docs/specs/`).

## The Causality Chain (10 hops)

```text
[H1]  operation_id ──bind── genesis record (step_seq=0)
[H2]  operation ──spawn──▶ task        (TaskLaunch parent→child, LaunchToken idempotent)
[H3]  task ──retry──▶ attempt          (ChildCompleted{task_id, attempt_id})
[H4]  input_id ──append──▶ record{step_seq, previous_record_digest}   (hash chain)
[H5]  input ──plan──▶ effect           (causation_input_id)
[H6]  effect ──resolve──▶ ResolveEffect{effect_id, outcome}           (accept_outcome shape check)
[H7]  effect ──host──▶ ProviderAttempt (primary key effect_id + attempt_seq)
[H8]  attempt ──bind──▶ request_fingerprint
[H9]  request ──respond──▶ response_id
[H10] journal ⟷ SessionLog             (join key = effect_id; number spaces stay independent)
```

H1–H6 close inside the kernel journal (State Truth). H7–H9 are the host execution-evidence chain (Evidence Truth), continuing from the single suture point effect_id — **the kernel chain is not extended; the host chain continues from effect_id** (D3). H10's join key is effect_id; **step_seq never enters SessionLog** (protecting S2's number-space independence). When old logs lack join fields, the validator degrades with an annotation (C7) instead of failing.

## Mixed-Batch Publication Constitution (M1–M5)

The publication discipline when a turn mixes syscalls with host tools (the root-cause layer of the 0.2.62 incident; the mechanisms live in crates/deepstrike-core/src/runtime/kernel/wire/driver/):

| # | Article | Meaning |
|---|---|---|
| M1 | effects XOR terminal | A step publishes effects or a terminal, never both |
| M2 | At most one pending effect per kind | The admissibility premise of effect settlement |
| M3 | A tool batch never co-publishes with syscall effects | Co-publishing hands the host a two-effect step, and the batch cannot be re-derived afterwards. Once syscall effects settle, resume **re-derives** the batch |
| M4 | While sibling effects are outstanding, the turn does not resume early | Non-empty pending → AwaitingResume; the last effect to settle re-runs resume with a free hand |
| M5 | The causation ledger settles last | A transition that faults mid-way must not leave a half-swallowed state in the journal |

The re-derive determinism of M3/M4 is the premise that makes J4 (steps stay out of the journal; only step_digest is stored) legal — validator rule C3 gates both at once.

## The Five Admission Gates (ToolCall → KernelEffect)

Only an intent that passes all five gates becomes a KernelEffect (effect_id can only be kernel-minted):

```text
ProviderCompleted.message.tool_calls  (Fact payload)
  ▼ Gate 1 attribution    three refusals: unknown effect / unexposed tool / consumed call_id
                          (caller derived by the kernel, B9)
  ▼ Gate 2 decode         malformed arguments = Rejection (an audit fact), never a fault
  ▼ Gate 3 privilege family  quarantine: a task that read untrusted content may not expand
                          power through privileged families — the whole family fails closed
  ▼ Gate 4 governance     capability lease precedes the governance pipeline;
                          governance evaluates the logical caller identity
  ▼ Gate 5 adjudication   syscall → kernel answers itself (AnsweredCall);
                          host tool → ExecuteTools effect
```

Syscalls (the 11 meta-tools) are adjudicated before the feed — they change what the next render contains; the assistant message is fed whole; host tools dispatch with the engine phase.

## Host Answer Obligations (DEC-7 / DEC-8)

- **DEC-7**: every published effect must be answered via `ResolveEffect{effect_id, outcome}`. Shape check: Failed is always admissible; Succeeded must match the shape of the published kind. No answer = the operation hangs (the kernel would rather hang than guess).
- **DEC-8**: host_effect_support is declared first (frozen into resolved_config at ConfigureOperation); an undeclared capability takes the downgrade path (the audit fact is identical for "the host said it can't" and "the host couldn't").
- The failure-fact vocabulary is controlled: an exhausted transport ladder → `TransportExhausted` ("The kernel never saw the backoff"); vendor raw text never crosses the boundary (B1). **A failure is a Fact too, journaled with the same weight as success.**

## Chain-Validator Rules (C1–C8)

Validator input = journal prefix + (optional) checkpoint + (optional) SessionLog. Checks run per chain segment, each segment reported independently; exit codes 0 = green / 1 = violation / 2 = input unparseable. Home: a core library module plus the `deepstrike inspect|verify|replay|fork` command (C3 is essentially "replaying the kernel with itself" and cannot live elsewhere; the validator is a host ops tool, not an SDK runtime path).

| # | Rule | Articles enforced |
|---|---|---|
| C1 | Chain integrity: previous_record_digest links, step_seq strictly +1, genesis prev=None | J1/J5/J6 |
| C2 | Input idempotency: the same input_id never appears on two different records | K5 rule 7/10 |
| C3 | Causal closure: every effect_id referenced by a ResolveEffect is reproducible by re-planning earlier records (step_digest recomputation) | J4 + M3/M4 |
| C4 | Task lineage: every task's parent chain terminates at the genesis root; a LaunchToken is never reused for a different TaskLaunch | H2/H3 |
| C5 | Checkpoint consistency: triple digests; the tail replays from base to through_step_seq; covered_head validated at the install point | K3/K4/K5-rule2 |
| C6 | Evidence join: every ResolveEffect(CallProvider) has ≥1 llm_completed with the same effect_id; its fingerprint has a matching prompt_measured; provider_attempts correspond one-to-one with the effect chain | S4 + H7/H8 |
| C7 | Degraded annotation: when old-format logs lack fields, checks degrade instead of failing and the report marks degraded hops | S4 compatibility |
| C8 | Invocation-chain forgery detection: between adjacent effects of an invocation's effectChain there must be a Failed resolution input | H7 anti-forgery |

C3 doubles as the **regression gate for re-plan determinism**: any kernel change that breaks re-plan determinism turns C3 red. C2+C3 cover exactly the two failure surfaces of the 0.2.62 multi-effect incident — once the validator lands, that class of incident drops from "production brick" to "CI red".

The validator is the correctness-gate subset of the fork-replay lab and can land independently first.
