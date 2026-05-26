# DeepStrike Kernel ABI

## Status

Phase 1 complete.

Current implementation status:

- Core exposes versioned `KernelInput`, `KernelAction`, `KernelObservation`, and `KernelStep`.
- Rust, Node, Python, and WASM SDK runners are driven through `KernelRuntime.step()`.
- Node, PyO3, and WASM FFI expose JSON `step(input_json) -> step_json` plus read-side helpers needed by host runners.
- Node, PyO3, and WASM legacy direct runtime facades have been removed from the public binding surface.
- Core `LoopStateMachine` and `ContextManager` remain internal implementation details and white-box test targets.
- Golden ABI fixtures cover all four host bindings (`tests/fixtures/abi/`). Rust, Node, Python, and WASM each run round-trip deserialization tests against the same fixture set.

## Goal

Define the stable host/kernel contract used by Rust, Node, Python, and WASM SDKs.

The kernel owns agent semantics. SDKs own host effects. SDKs should feed versioned inputs into the kernel and execute versioned actions returned by the kernel.

## Version

Current ABI version: `1`.

Every top-level ABI payload carries a `version` field. Consumers must reject newer major versions they do not understand.

## JSON ABI as Long-Term FFI Boundary

**Decision:** JSON is the confirmed long-term cross-language boundary for the Node, Python, and WASM FFI layers.

Rationale:

- All three host environments (V8/Node, CPython, WASM runtime) handle JSON natively without codegen tooling.
- The `serde` tag convention (`"kind"` discriminant, `snake_case`) is stable and matches existing SDK consumers.
- Strongly typed generated bindings (e.g. `napi` struct derives, PyO3 `#[pyclass]`) would require regenerating bindings on every ABI addition; JSON lets the Rust type system remain authoritative while SDKs stay forward-compatible.
- Version field (`"version": 1`) gives the runtime a rejection path for future major breaks without requiring SDK recompiles.

**Schema freeze commitment:**

- Adding a new `KernelInputEvent` variant: add to Rust enum + re-export, update all four FFI `match` arms in one PR, add a fixture file, bump golden fixture tests.
- Removing or renaming a variant is a major version bump (`version: 2`) and requires a migration adapter in `KernelRuntime::step`.
- Optional fields may be added to existing variants without a version bump; consumers must tolerate unknown fields.

The `tests/fixtures/abi/` directory is the canonical schema snapshot. CI must pass round-trip fixture tests on all four platforms before any ABI-touching PR merges.

## Types

### KernelInput

SDK to kernel:

```rust
pub struct KernelInput {
    pub version: u32,
    pub event: KernelInputEvent,
}
```

Events:

| Event | Meaning |
|---|---|
| `set_tools` | Replace user tool schemas visible to the kernel |
| `set_available_skills` | Replace skill metadata used by the skill meta-tool |
| `set_memory_enabled` | Toggle the memory meta-tool |
| `set_knowledge_enabled` | Toggle the knowledge meta-tool |
| `set_plan_tool_enabled` | Toggle the plan/update meta-tool |
| `set_tokenizer` | Select the tokenizer used by kernel token accounting |
| `add_system_message` | Add a system partition message before run start |
| `add_memory_message` | Add a memory partition message before run start |
| `add_history_message` | Add one history message |
| `preload_history` | Preload restored transcript and set replay baseline |
| `mount_capability` | Add a capability descriptor to the runtime graph |
| `unmount_capability` | Remove a capability descriptor by `capability_kind`/id |
| `load_milestone_contract` | Load milestone phases before run start |
| `force_compact` | Force an immediate context compact attempt |
| `update_task` | Apply a task-state update, typically from the plan meta-tool |
| `start_run` | Start a new run from a `RuntimeTask` |
| `resume` | Resume after preloaded history |
| `provider_result` | Feed an assistant/provider message back to the kernel |
| `tool_results` | Feed completed tool results back to the kernel |
| `signal` | Feed an external runtime signal |
| `milestone_result` | Feed verifier output for the current milestone |
| `timeout` | Terminate or interrupt by timeout |

### KernelAction

Kernel to SDK:

| Action | Host responsibility |
|---|---|
| `call_provider` | Call the configured LLM provider with rendered context and tools |
| `execute_tool` | Execute requested tool calls through the host execution plane |
| `evaluate_milestone` | Run a verifier and return `milestone_result` |
| `done` | Persist terminal state and stop the run |

### KernelObservation

Kernel audit source:

| Observation | Meaning |
|---|---|
| `compressed` | Context compression occurred |
| `renewed` | Context renewal started a new sprint |
| `rollbacked` | Fatal turn rollback restored checkpoint state |
| `capability_changed` | Runtime capability graph changed |
| `milestone_advanced` | Milestone passed and unlocked capabilities |
| `milestone_blocked` | Milestone failed and run should continue or retry |

## KernelRuntime

Phase 1 introduces a pure wrapper:

```rust
pub struct KernelRuntime;

impl KernelRuntime {
    pub fn step(&mut self, input: KernelInput) -> KernelStep;
}
```

`KernelStep` contains one or more actions plus observations emitted during the step.

Current implementation is intentionally thin over `LoopStateMachine`. This preserves behavior while giving FFI bindings a stable target. Host SDK runners should treat `KernelRuntime.step()` as the runtime control-plane boundary.

Read-side helpers exposed for SDK bookkeeping:

| Helper | Purpose |
|---|---|
| `turn()` | Audit/session event turn attribution |
| `recoveryContentBytes()` | Replay repair and truncation budget |
| `render()` | Reactive compact retry context |
| `drainNewMessages()` | Dream/session persistence |
| `preservedRefs()` | Compression audit metadata |

## Migration Status

1. [x] Core defines ABI types and `KernelRuntime`.
2. [x] Configuration, preload, capability, task update, tokenizer, and loop transitions are expressible as `KernelInputEvent` variants.
3. [x] FFI bindings expose `KernelRuntime.step()` and ABI payloads.
4. [x] Rust SDK runner migrated from direct `LoopStateMachine` calls to `KernelRuntime::step`.
5. [x] Node SDK runner migrated from direct runtime calls to `KernelRuntime.step()`.
6. [x] Python SDK runner migrated from direct runtime calls to `KernelRuntime.step()`.
7. [x] WASM SDK runner migrated from direct runtime calls to `KernelRuntime.step()`.
8. [x] Direct `LoopStateMachine` / `ContextManager` / legacy runtime access becomes internal, deprecated, or test-only.
9. [x] Golden ABI fixtures cover all four host bindings (`tests/fixtures/abi/`).

## Compatibility Rules

- Add optional fields rather than changing existing field meaning.
- Add new enum variants only with exhaustive match updates across Rust, Node, Python, and WASM in the same PR stack.
- Keep old behavior behind the ABI wrapper until all SDKs are migrated.
- Audit semantics come from kernel observations, not SDK-invented event names.
