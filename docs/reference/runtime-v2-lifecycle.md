# Runtime v2 Lifecycle Event Vocabulary

## Status

Draft. Runtime v1 events remain frozen; Runtime v2 is additive and should be
negotiated explicitly by SDKs that can consume the richer lifecycle stream.

## Goals

- Give SDKs a shared event vocabulary for agent lifecycle, governance, context
  renewal, capability changes, and cleanup.
- Separate recovery events from telemetry events so supervisors know which
  events require state restoration.
- Preserve the zero-I/O kernel boundary: the core defines event meaning, while
  SDKs decide transport, persistence, and UI rendering.

## Compatibility Rules

- Runtime v1 event names and payloads are not renamed or removed.
- Runtime v2 consumers must tolerate unknown event names.
- Runtime v2 events may be mirrored into v1-compatible telemetry, but v1 replay
  must not depend on v2-only fields.
- Every recoverable event should include stable `run_id`, `agent_id`, and
  `session_id` fields when emitted by an SDK.

## Event Vocabulary

| Event | Purpose | Category | Required fields |
| --- | --- | --- | --- |
| `agent_started` | Records a root agent or sub-agent run beginning. | Recovery | `run_id`, `agent_id`, `session_id`, `role`, `isolation` |
| `agent_suspended` | Records a run yielding control while preserving resumable state. | Recovery | `run_id`, `reason`, `resume_token` |
| `agent_resumed` | Records a suspended run being rehydrated. | Recovery | `run_id`, `resume_token` |
| `agent_finished` | Records terminal success, failure, or cancellation. | Recovery | `run_id`, `status` |
| `permission_requested` | Records a governance decision that requires human or policy approval. | Recovery | `run_id`, `tool_call_id`, `reason`, `stage` |
| `permission_resolved` | Records the answer to a prior permission request. | Recovery | `run_id`, `tool_call_id`, `decision` |
| `tool_denied` | Records a monotonic denial from governance. | Recovery | `run_id`, `tool_call_id`, `stage`, `reason` |
| `hook_applied` | Records a hook mutation or observation around a tool call. | Telemetry | `run_id`, `tool_call_id`, `hook_name`, `effect` |
| `capability_changed` | Records model-visible capability inventory changes. | Recovery | `run_id`, `change_kind`, `capability_id` |
| `context_renewed` | Records context renewal, compaction, or handoff artifact creation. | Recovery | `run_id`, `renewal_kind`, `snapshot_hash` |
| `background_task_detached` | Records work delegated outside the foreground loop. | Recovery | `run_id`, `task_id`, `reason` |
| `cleanup_started` | Records deterministic cleanup beginning. | Telemetry | `run_id`, `scope` |
| `cleanup_completed` | Records cleanup completion and residual state. | Recovery | `run_id`, `scope`, `status` |

## Recovery Semantics

Recovery events are replay inputs. A supervisor may use them to restore a run,
resume approval flow, rebuild a capability manifest, or decide whether cleanup
has completed. Telemetry events are useful for observability but should not be
required for correctness.

Runtime v2 replay should rebuild these kernel-adjacent objects:

- Agent identity, role, isolation, and resumable run status.
- Outstanding permission requests and their resolved decisions.
- Effective capability manifest version or hash.
- Latest context snapshot hint and renewal handoff metadata.
- Detached background task handles owned by the SDK.

## Mapping to Kernel Abstractions

- `agent_started` and `agent_resumed` map to `AgentRunSpec` and
  `AgentIdentity`.
- `permission_requested`, `permission_resolved`, and `tool_denied` map to
  `ToolDecisionPipeline` verdicts.
- `capability_changed` maps to `CapabilityManifest` updates.
- `context_renewed` maps to `ContextSnapshotHint` and renewal handoff artifacts.
- Cleanup events remain SDK-owned because the kernel performs no I/O.
