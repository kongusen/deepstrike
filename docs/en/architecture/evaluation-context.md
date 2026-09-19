# Context Contract in the Evaluation Runtime

In the Evaluation Runtime, Context is more than a compressed string or a token-pressure mechanism.
It is a verifiable projection of an evaluation input. A run must be able to answer which policy an
operation used, which input it accepted, which prompt was rendered, and which provider measurement
was applied.

## Authority boundaries

- The kernel owns `ContextPartition`, the resolved `ContextPolicy`, accepted input order, and durable state.
- The renderer is a Projection. It derives provider-facing context from canonical state and cannot become
  a second Context authority.
- `ContextTokenEngine` and host provider usage are Measurements. A measurement affects budgets or
  accounting only after normalize and Settlement.
- Raw context bytes, provider requests, and usage remain on the host evidence plane instead of becoming
  large objects in the Evolution ledger.

## EvaluationContextBinding

Every evaluated operation must have one `EvaluationContextBinding` in `EvaluationRun.contexts`. It binds
these digests:

| Field | Meaning |
| --- | --- |
| `context_policy` | Identity of the resolved context policy |
| `input_snapshot` | Canonical input snapshot being evaluated |
| `rendered_snapshot` | Provider-facing projection produced by the renderer |
| `prompt_measurement` | Prompt-token or provider-usage measurement |
| `cache_prefix` | Optional cache-prefix identity; when present it also requires evidence |

The binding digest also includes `operation_id`. The core E1–E8 validator rejects a tampered binding, a
binding for an unknown operation, an evaluation that leaves an operation unbound, and an evaluation that
does not list the binding evidence in `EvaluationRun.evidence_refs`.

Evaluation comparisons therefore pin the same dataset, artifact set, and Context bindings. Changing the
policy, recalled content, rendered projection, or measurement provider creates a new binding; changing
only a token number cannot reuse the old `EvaluationRun`.

## Replay relationship

Replay reads durable inputs and host evidence, regenerates the renderer projection, and compares its
digests with `EvaluationContextBinding`. SDKs expose mirrors and host-store adapters only; Node, Python,
and WASM delegate to the Rust core canonical validator. Context bytes may live in different stores, but
the binding fields, digest calculation, and evidence requirements are shared by every SDK.

Implementation references: [Evolution Runtime](./evolution-runtime), [Runtime Language](./runtime-language),
[ADR-011](../decisions/011-evaluation-context-contract.md).
