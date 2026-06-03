# SDK OS Parity Matrix

Cross-language checklist for the **Agent OS native profile** (0.2.6+). Every `RuntimeRunner` run loads default governance and in-kernel signal routing unless you override `governancePolicy` / `attentionPolicy`.

Optional `osProfile: "native"` / `os_profile: "native"` adds **fail-fast static validation** (required policies, no legacy governance instance). Behavioral defaults already match native semantics.

## Capability matrix

| Capability | core | node | python | rust | wasm |
| --- | --- | --- | --- | --- | --- |
| Default native governance + attention | ✓ | ✓ | ✓ | ✓ | ✓ |
| `attentionPolicy` + `signal_disposed` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `governancePolicy` suspend / resume | ✓ | ✓ | ✓ | ✓ | ✓ |
| `agent_process_changed` (proc) | ✓ | ✓ | ✓ | ✓ | ✓ |
| `schedulerBudget` / `scheduler_budget` → `set_scheduler_budget` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `resourceQuota` / `resource_quota` → `set_resource_quota` (M2) | ✓ | ✓ | ✓ | ✓ | ✓ |
| `memoryPolicy` / `memory_policy` → `set_memory_policy` (enforced) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Public OS config shape (`osProfile`, governance, attention, scheduler, quota) | ✓ | ✓ | ✓ | ✓ | ✓ |
| mm page-in before `execute_tool` | ✓ | ✓ | ✓ | ✓ | ✓ |
| Layer-1 `large_result_spooled` + spool I/O | ✓ | ✓ | ✓ | ✓ | event only |
| Semantic `page_out` → DreamStore | ✓ | ✓ | ✓ | ✓ | partial |
| `writeMemory` / `queryMemory` syscalls | ✓ | ✓ | ✓ | ✓ | event only |
| Kernel session events + `category` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `rebuild_os_snapshot_from_events` | ✓ | ✓ | ✓ | — | ✓ |
| `osProfile: "native"` / `os_profile: "native"` fail-fast | — | ✓ | ✓ | ✓ | ✓ |

**Notes**

- **Rust:** OS snapshot rebuild helpers are session-log oriented; runner exposes `write_memory` / `query_memory` parity with Node/Python.
- **WASM:** Memory syscall **session event types** are mapped; runner-level `writeMemory` / `queryMemory` APIs are not yet public.
- **Resource quotas (M2):** the kernel enforces spawn/depth/write-rate quotas for *all* SDKs (the `set_resource_quota` JSON event + `gate_syscall` trap are in core). Node/WASM expose `RuntimeOptions.resourceQuota`; Python/Rust expose `RuntimeOptions.resource_quota`. Each maps the ergonomic runner option onto the same snake_case kernel event.
- **Memory policy:** all four SDKs expose the ergonomic option (`RuntimeOptions.memoryPolicy` in Node/WASM, `RuntimeOptions.memory_policy` in Python/Rust) → the `set_memory_policy` JSON event, which the kernel **enforces** at the memory syscall traps: `validationEnabled: false` admits writes without validation, `maxContentBytes` / `maxNameLength` override the validation size limits, and `retrievalTopK` caps the emitted `requested_k` (`min(query.top_k, retrievalTopK)`). `memoryPath` / `staleWarningDays` are carried for the SDK's recall I/O (the kernel performs no recall I/O). Opt-in: with no policy installed, writes use default-rule validation and retrieval uses the requested top-k verbatim. Node/Python/Rust install the policy on both the run runtime and standalone memory-syscall runtimes; WASM installs it during run setup.
- **Public shape:** Node/WASM keep JS-style camelCase (`osProfile`, `governancePolicy`, `attentionPolicy`, `schedulerBudget`, `resourceQuota`); Python/Rust keep native snake_case (`os_profile`, `governance_policy`, `attention_policy`, `scheduler_budget`, `resource_quota`). The emitted kernel events are the same snake_case JSON ABI.

## CI gates

- **Static parity:** `node scripts/check-sdk-parity.mjs` (markers in each SDK tree).
- **Integration:** `node/tests/runtime/os-native-profile.test.ts`, `node/tests/runtime/memory-syscall.test.ts`, `python/tests/test_os_native_profile.py`, `python/tests/test_memory_syscall.py`, Rust runner tests.
- **Golden OS snapshot:** `tests/fixtures/session/*.json` + `tests/rust` `t16_os_snapshot_golden`, `node/tests/runtime/os-snapshot-golden.test.ts`.

## Native profile requirements

When `osProfile: "native"` is set explicitly:

| Option | Required |
| --- | --- |
| `attentionPolicy` / `attention_policy` | Yes — in-kernel signal routing |
| `governancePolicy` / `governance_policy` | Yes — in-kernel syscall gate |
| Legacy `governance` instance on runner | Forbidden |
| Legacy `COMPAT(signal-legacy)` router | Forbidden (fail-fast) |

**0.2.6 default runs** satisfy these behaviorally via `DEFAULT_NATIVE_ATTENTION_POLICY` and `DEFAULT_NATIVE_GOVERNANCE_POLICY` without setting `osProfile`.

See [Kernel ABI — OS Native Profile](./reference/kernel-abi.md#os-native-profile-phase-6) and [Agent OS](./concepts/agent-os.md).
