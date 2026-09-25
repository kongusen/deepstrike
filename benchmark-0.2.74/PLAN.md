# Implementation Plan: Node SDK 0.2.74 Benchmark

## Architecture Decisions

1. Keep `benchmark-0.2.74/` as the repository benchmark tree for SDK 0.2.74.
2. Treat the package exports map as the benchmark's contract boundary. Load root plus every declared 0.2.74 subpath through one adapter; never import implementation files from scenarios.
3. Build the benchmark in three layers: public-surface contracts, behavior scenarios, and replay/regression artifacts. Deterministic fixtures validate the current SDK without API keys.
4. Keep metrics and golden logic provider-agnostic so replay and live runs share the same artifact format.

## Task List

### Phase 1: Public contract foundation

- [ ] SDK compatibility loader, version guard, and public-barrel contract manifest.
  - Acceptance: root plus `providers`, `workflow`, `planes`, `memory`, `harness`, `os`, `advanced`, `runtime`, and `evals` resolve from the 0.2.74 export map; forbidden root leaks are reported.
  - Verify: contract conformance test.
- [x] Layered contract manifest and exact export-map guard.
  - Acceptance: every contract is assigned to a known layer; undeclared package subpaths fail the contract report.
  - Verify: contract conformance test with injected undeclared path.
- [ ] MetricSet, diff, render, golden utilities.
  - Acceptance: deterministic JSON schema, diff output, tolerance checks.
  - Verify: unit tests.

### Checkpoint: Foundation

- [ ] `npm test --prefix benchmark-0.2.74` passes.
- [ ] `node benchmark-0.2.74/cli/bench.mjs list` works.

### Phase 2: Contract-backed behavior evaluation

- [ ] Agent facade scenario.
  - Acceptance: root `createAgent` validates run/stream/session/resume/interrupt/result/evidence and memory boundaries through public bindings.
- [ ] Workflow and dynamic workflow scenarios.
  - Acceptance: `RuntimeRunner.runWorkflow` and `runDynamicWorkflow` cover scheduler, limits, paired lifecycle, replay reuse, and mismatch.
- [ ] Planes, harness, and eval scenarios.
  - Acceptance: each public surface has at least one deterministic behavior assertion, with no provider network calls.
- [ ] Run artifact and replay fixture support.
  - Acceptance: every sample is addressable and replayed deterministically.
  - Verify: no-network CLI run and JSON diff.

### Phase 3: Regression and documentation

- [ ] Golden save/check and baseline layout.
- [ ] README with commands, metrics, limitations, and 0.2.74 API map.
- [ ] Optional live provider adapter contract, disabled by default.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| SDK subpath export drift | High | Surface smoke test imports every required public barrel. |
| Workflow outcomes vary with kernel changes | Medium | Assert structural invariants and store full event traces. |
| Replay fixture names collide across samples | High | Persist a run manifest with sample/task mapping. |
| Live model noise contaminates mechanism metrics | High | Replay by default; require explicit `--mode live`. |

## Checkpoints

1. Foundation compiles and unit tests pass.
2. Workflow variants run without an API key.
3. Golden check passes against a saved deterministic baseline.
