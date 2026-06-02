# SDK OS Parity Matrix

Cross-language checklist for the **Agent OS native profile** (0.2.5+). Every `RuntimeRunner` run loads default governance and in-kernel signal routing unless you override `governancePolicy` / `attentionPolicy`.

Optional `osProfile: "native"` / `os_profile: "native"` adds **fail-fast static validation** (required policies, no legacy governance instance). Behavioral defaults already match native semantics.

## Capability matrix

| Capability | core | node | python | rust | wasm |
| --- | --- | --- | --- | --- | --- |
| Default native governance + attention | ✓ | ✓ | ✓ | ✓ | ✓ |
| `attentionPolicy` + `signal_disposed` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `governancePolicy` suspend / resume | ✓ | ✓ | ✓ | ✓ | ✓ |
| `agent_process_changed` (proc) | ✓ | ✓ | ✓ | ✓ | ✓ |
| mm page-in before `execute_tool` | ✓ | ✓ | ✓ | ✓ | ✓ |
| Layer-1 `large_result_spooled` + spool I/O | ✓ | ✓ | ✓ | ✓ | event only |
| Semantic `page_out` → DreamStore | ✓ | ✓ | ✓ | ✓ | partial |
| `writeMemory` / `queryMemory` syscalls | ✓ | ✓ | ✓ | ✓ | event only |
| Kernel session events + `category` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `rebuild_os_snapshot_from_events` | ✓ | ✓ | ✓ | — | ✓ |
| `osProfile: "native"` fail-fast | — | ✓ | ✓ | — | ✓ |

**Notes**

- **Rust:** OS snapshot rebuild helpers are session-log oriented; runner exposes `write_memory` / `query_memory` parity with Node/Python.
- **WASM:** Memory syscall **session event types** are mapped; runner-level `writeMemory` / `queryMemory` APIs are not yet public.

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

**0.2.5 default runs** satisfy these behaviorally via `DEFAULT_NATIVE_ATTENTION_POLICY` and `DEFAULT_NATIVE_GOVERNANCE_POLICY` without setting `osProfile`.

See [Kernel ABI — OS Native Profile](./reference/kernel-abi.md#os-native-profile-phase-6) and [Agent OS](./concepts/agent-os.md).
