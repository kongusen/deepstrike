# 0.2.70 Evolution Runtime

0.2.70 advances the Framework Verifiable Runtime into an Evolution Runtime. The framework can prove which artifact set produced a new operation, why a candidate was proposed, which evaluation evidence was used, and why a promotion decision allowed activation.

```text
ArtifactVersion
  → EvolutionProposal
  → EvaluationRun / EvaluationFact
  → PromotionDecision
  → ArtifactSet activation
  → Verifiable Operation
```

Artifact bytes remain in an immutable host-owned CAS. The kernel receives only verified content-addressed references and fixes `artifact_set_digest` plus the promotion-decision reference in operation genesis. A running operation never hot-swaps its artifact set; activation begins at a new operation boundary.

Evolution objects are authoritative in the host ArtifactStore, EvaluationStore, and EvolutionLedger. The kernel owns canonical bytes, digest integrity, activation causality, and replay binding. SessionLog, Checkpoint, SDK mirrors, and reports are evidence or projections and cannot become a second semantic authority for proposals, artifacts, or promotion.

The kernel ABI is the sole supported contract. Earlier journal, checkpoint, report, and evolution formats have no negotiation, shape inference, or migration path; deployments that must continue old data stay on 0.2.69. The E1–E8 validator rejects tampered digests, broken lineage, incorrect proposal bindings, incomplete evidence, invalid regressions, unsatisfied gates, and activation outside the declared boundary.

Implementation references: [0.2.70 specification](../../.local-docs/specs/runtime-evolution-0.2.70.md) · [ADR-010](../decisions/010-evolution-runtime-hard-cut.md) · [Framework Verifiable Runtime](./verifiable-runtime)
