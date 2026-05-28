# SDK Kernel Driver Parity Plan

## Status

Draft implementation plan for the v0.2.x SDK alignment work.

## Context

DeepStrike v0.2.0 reframes the runtime as an agent OS microkernel:

- The kernel owns agent semantics: state machine, context VM, capability bus, governance, transactions, milestones, sub-agent isolation, audit semantics, and the host ABI.
- SDKs own host effects: provider calls, tool execution, filesystem/process/network access, UI and human approval, session storage, archive storage, and orchestration glue.

The SDKs should therefore behave like kernel drivers. They feed versioned `KernelInput` into `KernelRuntime.step()`, execute returned `KernelAction`s, persist `KernelObservation`s, and avoid inventing runtime behavior outside the kernel.

Node is currently the closest reference SDK. Python, Rust, and WASM should align with the same public contract where their host environment supports it.

This plan also incorporates the external SDK adjustment analysis from `sdk_adjustment_analysis.md`, especially its six alignment dimensions: control flow, Context VM, Security LSM, transaction runtime, milestone contracts, and sub-agent isolation.

## Non-Goals

- No `KernelInput` ABI version bump for this plan.
- No removal of deprecated ABI variants during the parity work; old variants can remain for compatibility and fixtures.
- No provider behavior rewrite unless needed to execute an existing kernel action.
- No attempt to make every host support identical I/O features. Browser/WASM may expose narrower host effects, but the kernel action semantics should stay consistent.

## Design Principles

1. Keep the kernel as the only control plane.
2. Add optional SDK fields instead of changing existing public field meanings.
3. Treat Node as the reference shape, not as a source of Node-only semantics.
4. Prefer `capability_command` over deprecated capability events for all new SDK code.
5. Persist kernel observations as the audit source of truth.
6. Make unsupported host effects explicit in SDK docs rather than implying parity.

## Parity Matrix

| Capability | Node | Python | Rust | WASM | Target |
| --- | --- | --- | --- | --- | --- |
| `RuntimeRunner.run` / `wake` | Present | Present | Present | Present | Keep aligned |
| Kernel-driven provider loop | Present | Present | Present | Present | Keep aligned |
| Kernel-driven tool execution | Present | Present | Present | Present | Keep aligned |
| `milestonePolicy` / equivalent | Present | Present | Present | Present | Aligned |
| Milestone verifier callback | Present | Present | Present | Present | Aligned |
| Milestone contract loading | Present | Present | Present | Present | Aligned |
| `runSpec` / `AgentRunSpec` on `start_run` | Present | Present | Present | Present | Aligned |
| Dynamic capability mount via `capability_command` | Present | Present | Not exposed | Not exposed | Aligned where helpers exist |
| Public `push_artifact` helper | Removed | Removed | Removed | — | **Removed** — use Slot 2 / history compression |
| `initialMemory` / `initial_memory` → Slot 2 | Present | Present | Present | Present | Aligned |
| `ArchiveStore` integration for compressed history | Present | Present | Present | Present | Verified by replay tests |
| `ToolErrorKind` / fatal rollback inputs | Present | Present | Present | Present | Verified by tests |
| Rollback observation repair / log truncation | Present | Present | Present | Present | Verify tests across SDKs |
| Permission request / human approval events | Present | Present | Present | Present | Verify host UX docs and tests |
| Sandbox profile enforcement | Partial host enforcement | Partial host enforcement | Partial host enforcement | Host-limited | Document limits; harden incrementally |
| Active parent sub-agent spawn | Present | Present | Planned v0.3.0 | Present | Rust deferred |
| Sub-agent harness (`subAgentHarness` / `sub_agent_harness`) | Present | Present | — | — | Aligned (Node + Python) |
| Standalone sub-agent spawn | Present | Present | Planned v0.3.0 | Present | Rust deferred |
| Observation persistence for kernel audit | Present | Present | Present | Present | Keep aligned |
| Golden ABI fixture round-trip | Present | Present | Present | Present | Keep as CI gate |
| `done.status = milestone_pending` docs | Present | Present | Present | Present | Aligned |

## Alignment Dimensions

### Control Flow

SDK runners should only implement the host driver loop:

1. Send `KernelInput`.
2. Receive `KernelAction`.
3. Execute the requested host effect.
4. Feed the result back to the kernel.
5. Persist `KernelObservation`s.

SDKs must not independently decide loop termination, context layout, capability visibility, milestone progression, or rollback policy except where they are executing an explicit kernel action.

### Context VM and Archive Store

SDKs should treat the four kernel slots as internal VM state (`system`, `knowledge`, `task_state` + `signals`, `history`). Host APIs may send `add_system_message`, `add_knowledge_message`, `preload_history`, and `update_task`, but should not reconstruct provider prompts outside `call_provider.context`.

`call_provider.context` exposes `system_stable`, `system_knowledge`, and `turns` — map these to provider-specific layouts (Anthropic: two cached system blocks + messages; OpenAI: `system_text` + messages).

Compressed history is a host storage responsibility. When the kernel emits `compressed`, SDKs should persist archived messages through `ArchiveStore`; replay should restore archive references without re-inlining large blobs.

### Security LSM and Sandbox

Tool execution is a syscall-like host effect. The kernel owns the decision pipeline; SDKs enforce the result in their `ExecutionPlane`.

Host SDKs must surface permission requests to the configured UI/console/human approval path. They should not bypass kernel governance by directly executing sensitive tool calls.

Sandbox support is host-specific. The parity target is first to document limits and preserve kernel sandbox metadata. Later hardening can move from hygiene checks to stronger OS-enforced isolation where available.

### Transaction Runtime

SDKs should preserve `ToolErrorKind` and fatal/non-fatal tool result metadata when feeding `tool_results`. Recoverable errors remain model-visible; fatal errors let the kernel decide rollback.

Rollback repair belongs at the host persistence boundary. When the kernel emits `rollbacked`, SDKs should keep `SessionLog` replayable by truncating or repairing local events according to kernel observations.

### ABI Fixtures

`tests/fixtures/abi/` remains the canonical schema snapshot. Any new optional field or enum variant handling should update fixture coverage and keep Node, Python, Rust, and WASM round-trip tests passing.

## Phase 1: Milestone Contract Parity

### Objective

Make `evaluate_milestone` a cross-SDK host responsibility: run a verifier when configured, feed `milestone_result` back to the kernel, and otherwise stop with `milestone_pending`.

### Node

Node is the reference. Preserve the current shape:

- `milestonePolicy`
- `onMilestoneEvaluate`
- `milestoneContract`
- `runSpec`

### Python

Add to `RuntimeOptions`:

- `milestone_policy`
- `on_milestone_evaluate`
- `milestone_contract`
- `run_spec`

Update `KernelRunnerAction` parsing to include:

- `verifier`
- `required_evidence`

Update `evaluate_milestone` handling:

- `auto_pass` feeds `milestone_result`.
- verifier callback feeds `milestone_result`.
- default path emits terminal `milestone_pending`.

### Rust

Add to `RuntimeOptions`:

- `milestone_contract`
- `run_spec`
- verifier callback or trait object, for example `MilestoneVerifier`

Update `StartRun` to pass `run_spec` when configured.

Update `EvaluateMilestone` handling:

- `AutoPass` remains test-only / explicit.
- verifier callback feeds `MilestoneResult`.
- default path emits terminal `milestone_pending`.

### WASM

Add browser-friendly equivalents:

- `milestonePolicy`
- `onMilestoneEvaluate`
- `milestoneContract`
- `runSpec`

Default behavior may remain `milestone_pending` when no verifier is configured.

### Acceptance Criteria

- All four SDKs reject implicit auto-pass by default.
- All verifier callbacks receive phase id, criteria, verifier metadata, and required evidence.
- All verifier results are converted to `milestone_result`.
- Tests cover pass, fail, and pending paths.

## Phase 2: Capability Command Parity

### Objective

Stop public SDK helpers from emitting deprecated `mount_capability` / `unmount_capability` inputs.

### Work

- Add shared helper equivalents to each SDK:
  - `capability_command_mount`
  - `capability_command_unmount`
- Use default provenance:
  - `mounted_by = "sdk:runtime"`
  - `mount_reason = "dynamic_register"`
- Keep deprecated event parsing only for compatibility.

### Acceptance Criteria

- Dynamic tool, skill, marker, and MCP capability changes use `capability_command`.
- `capability_changed` observations include provenance fields where applicable.
- Existing deprecated ABI fixtures still pass.

## Phase 3: Transaction, Governance, and Sandbox Parity

### Objective

Verify that tool errors, permission requests, sandbox metadata, and rollback observations preserve kernel semantics across SDKs.

### Work

- Audit all `ExecutionPlane` implementations for `ToolErrorKind` propagation.
- Verify fatal tool errors feed `is_fatal` / `error_kind` into `tool_results`.
- Verify recoverable tool errors remain visible to the model instead of forcing host-side rollback.
- Add or update rollback replay tests for each SDK.
- Document current sandbox limits for Node, Python, Rust, and WASM.
- Track stronger OS-enforced sandbox options separately from the parity work.

### Acceptance Criteria

- Tool result metadata reaches the kernel consistently across SDKs.
- `rollbacked` observations leave `SessionLog` replayable.
- Permission request events are surfaced through SDK event streams.
- Sandbox limits are explicit in docs rather than implied to be stronger than they are.

## Phase 4: Artifact API Parity

> **Superseded by four-slot refactor.** The artifacts partition was removed. Large outputs stay in history (Snip/Micro compression tiers) or preload into Slot 2 via `add_knowledge_message` / `initial_memory`. The `push_artifact` ABI event is no longer handled by the kernel.

### Objective (historical)

Expose a cross-SDK way to push large host artifacts into the kernel artifacts partition instead of inlining them into history.

### Work (cancelled — cleanup done)

Phase 4 is cancelled. SDK `push_artifact` helpers removed from Python and Rust (Node/WASM never exposed a public helper). Replacement guidance:

- Durable preload → `add_knowledge_message` / `initialMemory` / `initial_memory` (Slot 2)
- Large in-run outputs → history with Snip/Micro compression tiers

### Acceptance Criteria (historical — not pursued)

- Each supported SDK can emit `push_artifact`.
- Artifact references survive replay.
- Large outputs do not re-enter normal history as inline blobs.

## Phase 5: Golden ABI and Fixture Gate

### Objective

Keep the JSON ABI as the shared, tested contract while SDK surfaces are aligned.

### Work

- Add fixture coverage for any newly consumed optional action fields.
- Ensure all SDKs tolerate unknown optional fields.
- Keep round-trip tests green for Node, Python, Rust, and WASM.
- Treat fixture drift as a blocker for SDK parity PRs.

### Acceptance Criteria

- Every ABI-touching parity PR updates fixtures when needed.
- All four SDK fixture tests pass.
- No SDK relies on undocumented action or observation quirks.

## Phase 6: Sub-Agent Surface Decision

### Objective

Make sub-agent support explicit across SDKs.

### Decision

**Rust SDK Sub-Agent Scope:** Document Rust sub-agent orchestration support as **planned for v0.3.0**. Keep Node, Python, and WASM as first-class implementations in v0.2.x.

Verify WASM:

- Active parent spawn calls kernel `spawn_sub_agent`.
- Standalone spawn feeds `sub_agent_completed`.
- Capability filtering follows the kernel isolation manifest.

### Acceptance Criteria

- README and SDK guides no longer imply unsupported sub-agent parity.
- Supported SDKs have sub-agent tests.
- Unsupported SDKs have explicit tracking notes.

## Phase 7: Documentation and Guides

### Objective

Refresh docs so the kernel-driver narrative matches the public SDK surface.

### Work

- Update `README.md`.
- Update SDK guides for Node, Python, Rust, and WASM where present.
- Update `docs/spec-kernel-abi.md` if parity decisions change the SDK entry-point table.
- Document `milestone_pending` as a valid terminal status.

### Acceptance Criteria

- Quick starts still focus on `RuntimeRunner`, `SessionLog`, and `ExecutionPlane`.
- Milestone, capability, artifact, and sub-agent sections describe host responsibilities, not SDK-owned semantics.
- Each SDK guide marks unsupported host features clearly.

## Recommended PR Stack

1. `docs: define sdk kernel-driver parity plan`
2. `feat(python): align milestone and capability command runtime`
3. `feat(rust): align milestone contract runner surface`
4. `feat(wasm): align milestone and artifact host surface`
5. `test(sdk): tighten tool error, rollback, and ABI fixture parity`
6. `feat(sdk): add artifact parity helpers`
7. `docs: refresh kernel-driver SDK guides`

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| SDK APIs drift while parity is in progress | Medium | Treat Node as reference and keep this matrix updated per PR |
| Rust active-run artifact API needs a larger runner refactor | Medium | Decide on `RunHandle` before implementation |
| WASM host limitations make parity confusing | Medium | Document unsupported host effects explicitly |
| Deprecated capability events remain observable | Low | Keep compatibility, but stop public helpers from emitting them |
| Sandbox wording overpromises host isolation | Medium | Separate current hygiene checks from future OS-enforced hardening |
| Tool error metadata is dropped by a host plane | High | Add focused tests around `ToolErrorKind`, `is_fatal`, and rollback |

## Open Questions & Decisions

- **Rust Sub-Agent Support**: Marked as planned for v0.3.0.
- **Artifact Push Timing**: Allowed only during active runs.
- **Verifier Callback Arguments**: Receive a simple language-native parameter list or typed options object.
- **Milestone Pending Status**: Treated as a valid terminal status (`status: "milestone_pending"`) in v0.2.x to suspend active runner threads gracefully.
- **Sandbox Boundary**: SDKs enforce soft hygiene rules (e.g. preventing file paths leaving the workspace). Hard sandbox isolation (CPU/Memory limits, full network isolation, syscall blocking) is explicitly delegated to OS-enforced runtime containers (Docker, gVisor, WebAssembly VMs).
- **Tool Error Classification**: Default custom tool exceptions to `Recoverable` errors (injected in history for self-correction), but allow developers to raise explicit `FatalToolError` to trigger kernel rollback.
