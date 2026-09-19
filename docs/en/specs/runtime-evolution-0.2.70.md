# DeepStrike 0.2.70 — Evolution Runtime Hard Cut

## Status

Draft specification approved for implementation planning. This release intentionally replaces the
0.2.69 runtime and public contracts. It does not provide runtime compatibility with 0.2.69 data,
ABI, reports, or SDK constructors.

## Objective

0.2.70 turns the Framework Verifiable Runtime into an Evolution Runtime. A candidate artifact can
be proposed, evaluated, promoted, activated for a new operation, and replayed from immutable
evidence:

```text
ArtifactVersion
  → EvolutionProposal (Intent)
  → EvaluationRun / EvaluationFact (Evidence / Fact)
  → PromotionDecision (Decision)
  → ArtifactSet activation
  → Verifiable Operation
```

The framework must prove which artifact set produced an operation, why a candidate was proposed,
which evaluation evidence was used, and why the promotion decision was admitted. Artifact bytes
remain host-owned; the kernel receives only verified content-addressed references.

## Authority and identity

| Object | Domain | Authority | Durability | Identity | Causation | Replay |
| --- | --- | --- | --- | --- | --- | --- |
| `ArtifactVersion` | evolution/artifact | Host ArtifactStore | immutable CAS | content-addressed digest | parent artifact digests | manifest and payload digest |
| `ArtifactSet` | runtime binding | Host store, bound by kernel genesis | immutable CAS | content-addressed digest | artifact version digests | resolved artifact set in genesis |
| `EvolutionProposal` | evolution/intent | Host EvolutionLedger | append-only | content-addressed intent | base and candidate artifacts | canonical intent bytes |
| `EvaluationRun` | evolution/evidence | Host EvaluationStore | append-only | content-addressed run | proposal and evaluation plan | operation and evidence references |
| `EvaluationFact` | evolution/fact | Host EvaluationStore | immutable | content-addressed fact | evaluation run | normalized metrics and gate results |
| `PromotionDecision` | evolution/decision | Host EvolutionLedger | append-only | content-addressed decision | proposal, facts, policy | canonical decision bytes |
| operation activation | runtime/state | Kernel Journal | durable chain record | kernel-minted input/effect IDs | promotion decision and artifact set | genesis binding and checkpoint config |

No object may acquire a second semantic authority in SessionLog, Checkpoint, SDK mirrors, or a
report projection. SessionLog remains evidence about external evaluation activity; it does not own
the proposal, artifact, or promotion state.

## Canonical object contracts

### ArtifactVersion

An `ArtifactVersion` is immutable. Its canonical manifest contains the artifact kind, payload
digest, parent digests, producer/toolchain digest, and declared scope. The payload reference is
opaque to the kernel. A manifest whose digest does not match its canonical bytes is rejected.

Initial artifact kinds are `runtime`, `skill`, `prompt`, `policy`, `toolset`, and `bundle`. New
kinds require a vocabulary and replay rule before implementation.

### ArtifactSet

An `ArtifactSet` names every artifact that can affect a new operation. Its ordering and canonical
bytes are frozen. An unbound operation may omit `artifact_set_digest`; a promoted or replayed artifact
set must carry a verified digest, and an operation cannot mutate its artifact set after genesis.

### EvolutionProposal

The proposal binds one base artifact set to one candidate artifact set, a declared objective, a
change manifest, constraints, and a proposer identity. It is an Intent and cannot itself activate
an artifact. A proposal with missing parents, a changed base, or an unregistered artifact kind is
invalid.

### EvaluationRun and EvaluationFact

An evaluation run binds a proposal, baseline and candidate artifact sets, evaluator manifest,
dataset/fixture digest, operation IDs, one `EvaluationContextBinding` per operation, and evidence
references. A context binding covers the resolved context policy, canonical input snapshot, rendered
snapshot, prompt measurement, and optional cache prefix. Facts contain normalized metrics, required gate
outcomes, regression comparisons, and replay verdicts. Raw context bytes, provider usage, and wire
evidence stay on the host evidence plane.

### PromotionDecision

A promotion decision binds one proposal, its evaluation facts, and one content-addressed policy.
It records `promote`, `reject`, or `hold`, the required gates, the selected artifact set, and the
decision cause. A `promote` decision is invalid unless every required gate is present, replayable,
and passing. Activation only consumes a verified promotion decision.

## Evolution validation rules

The core evolution validator adds these fail-closed rules:

- **E1** — artifact and manifest digest integrity;
- **E2** — parent closure and acyclic lineage;
- **E3** — proposal base/candidate binding;
- **E4** — evaluation binding to the exact proposal and artifact sets;
- **E5** — evidence completeness, context coverage, and operation replayability;
- **E6** — baseline/candidate metric and regression validity;
- **E7** — promotion policy and required-gate satisfaction;
- **E8** — activation causality, boundary, and decision validity.

The ledger is reduced from immutable records. A mutable status field is never authoritative.

## Deliberate hard cut

0.2.70 uses a clean contract boundary despite retaining the `0.2.x` numbering:

- use one ABI identity;
- replace the 0.2.69 checkpoint and report contracts;
- reject all 0.2.69 and earlier journal/checkpoint/evolution formats with typed `unsupported_format`;
- remove compatibility free functions, old report schemas, and the `ds-chain-validator` entry point;
- remove first-party legacy SDK constructors and mirrors; current third-party protocol adapters remain;
- do not negotiate versions, infer formats from payload shape, or silently migrate data;
- require a new operation for an activated artifact set; in-flight hot replacement is out of scope.

Deployments that must continue old operations stay on 0.2.69. 0.2.70 has no runtime migration
path.

## Implementation order

1. Constitution, ABI/version rejection matrix, and negative fixtures.
2. Artifact CAS types and host stores.
3. Evolution Ledger and E1–E8 validator.
4. Evaluation runner, normalized facts, and deterministic replay integration.
5. Kernel genesis `ArtifactSet` binding and activation boundary.
6. Rust, Node, Python, and WASM framework mirrors.
7. CLI adapter, compatibility purge, docs, and release gates.

Every slice must leave the workspace buildable and add positive and negative fixtures before the
next slice changes a public contract.

## Commands and release gates

```text
cargo fmt --all -- --check
cargo test --workspace
cargo check --workspace
python/.venv/bin/python -m pytest -q python/tests
npm test --prefix node
npm test --prefix wasm
npm run docs:build
npm run docs:drift
npm run test:conformance
```

The release gate must prove old-format rejection, artifact tamper detection, lineage-cycle
rejection, missing-evidence rejection, promotion-gate rejection, and activation replay.

## Explicit non-goals

- Artifact bytes do not enter Kernel Journal or Kernel Checkpoint.
- Provider attempts, billing, pricing, and reasoning-token semantics do not enter core state.
- Promotion does not hot-swap a running operation.
- SessionLog does not become an evolution authority.
- Third-party protocol interoperability is not removed.
