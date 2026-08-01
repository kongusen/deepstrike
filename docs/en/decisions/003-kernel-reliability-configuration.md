# ADR-003: SDK Configuration Boundary for Kernel Reliability

## Status

Superseded by ADR-005

## Date

2026-07-14

## Context

superseded Kernel ABI moved replay, effect correlation, recovery, and result externalization into the kernel contract. ADR-005 replaces that wire; this ADR remains as the rationale for keeping host resource policy narrow.

## Decision

`RunConfig.reliability` carries one aggregated `KernelReliabilityConfig`. It exposes only parameters for which the host owns a resource or failure-policy responsibility:

- `event_replay_capacity`: deduplicated input-event window;
- `completed_effect_replay_capacity`: completed effect-result window;
- `provider_recovery_attempts`: provider context-overflow recovery attempts;
- `output_recovery_attempts`: truncated-output continuation attempts;
- `host_effect_retry_attempts`: retry attempts for host durability effects such as page-out;
- `max_input_bytes`: canonical JSON byte limit for one ABI input, default 16 MiB; typed and JSON entry points enforce the same boundary.
- `tail_bounds`: record/byte soft watermarks and hard limits for the journal tail between logical checkpoints.

Host storage locations are not kernel policy. Canonical hosts expose `PayloadStore`; locators remain opaque to core.

Host-side policies that affect resource use but are not executed by core do not masquerade as kernel parameters. Workflow structured-output validation attempts use `workflowSchemaValidationAttempts` in Node/WASM and `workflow_schema_validation_attempts` in Python. The allowed range is `1..=16`, with a default of `2`.

Configuration is validated as a whole and applied atomically at the ABI boundary. Replay-window capacities are limited to `1..=65536`; one input is limited to `256 B..=64 MiB`; and recovery attempts are at most `16`. Tail hard limits must be nonzero and no lower than their soft watermarks; defaults are 512/2048 records and 4/16 MiB for soft/hard limits. Omitted fields keep kernel defaults.

The kernel exposes a read-only `KernelDiagnostics` projection with input count/bytes, journal high-water state, replay/effect/pending counts, and lifecycle. It has no setters and cannot bypass versioned input transactions.

Checkpoint and canonical-record 64-bit values use decimal-string `WireU64` encoding so Node/WASM JSON round trips cannot lose `Number` precision.

Existing standalone policies keep their established entry points: the signal queue belongs to attention policy, while repeat fuse, entropy watch, scheduler budget, and resource quota are not duplicated in the reliability bundle.

Serialization versions, entropy formula constants, rendering previews, task-state display counts, short diagnostic text lengths, and safe-truncation algorithm details remain internal. They do not represent host resource commitments and must not become SDK compatibility contracts.

## Alternatives

### Add one `Set*` event per parameter

Rejected. This would reintroduce scattered configuration events and make cross-field validation and atomic application difficult.

### Expose every constant

Rejected. Observable implementation details become de facto APIs and obstruct later algorithm changes.

### Compile-time configuration only

Rejected. Deployment differences among Node, Python, and Rust SDKs occur per run and cannot be expressed by compile-time constants.

## Consequences

- SDKs can tune reliability memory bounds and recovery policy per run.
- Count and byte limits prevent one oversized payload from bypassing the checkpoint-tail resource boundary.
- Invalid combinations return `invalid_config` before any field takes effect.
- Checkpoints preserve the resolved policy and bounded replay content, so restore continues with the same limits.
- Node, Python, and Rust hosts map their public options to the bundle during the effect-protocol cutover.
