# SDK OS Parity Matrix (Phase 6)

Cross-language checklist for the **OS Native Profile**. Legacy (`osProfile: "legacy"`) remains the default; enable native only when all required policies are configured.

## Capability matrix

| Capability | core ABI | node | wasm | python |
|------------|----------|------|------|--------|
| `attentionPolicy` + `signal_disposed` | ✓ | ✓ | ✓ | ✓ |
| `governancePolicy` suspend / resume | ✓ | ✓ | ✓ | ✓ |
| `agent_process_changed` (proc) | ✓ | ✓ | ✓ | ✓ |
| mm page-in before `execute_tool` | ✓ | ✓ | ✓ | ✓ |
| kernel session events + `category` | ✓ | ✓ | ✓ | ✓ |
| `osProfile: "native"` fail-fast | — | ✓ | ✓ | ✓ |
| `rebuild_os_snapshot_from_events` | ✓ | ✓ | ✓ | ✓ |

## CI gates

- **Static parity:** `node scripts/check-sdk-parity.mjs` (markers in each SDK tree).
- **Integration:** `node/tests/runtime/os-native-profile.test.ts`, `wasm/tests/os-native-profile.test.ts`, `python/tests/test_os_native_profile.py`.
- **Golden OS snapshot:** `tests/fixtures/session/*.json` + `tests/rust` `t16_os_snapshot_golden`, `node/tests/runtime/os-snapshot-golden.test.ts`.

## Native profile requirements

| Option | Required when `native` |
|--------|-------------------------|
| `attentionPolicy` / `attention_policy` | Yes — in-kernel signal routing |
| `governancePolicy` / `governance_policy` | Yes — in-kernel syscall gate |
| Legacy `governance` instance | Forbidden |
| Legacy `COMPAT(signal-legacy)` router | Forbidden (fail-fast) |

See [kernel-abi.md](./reference/kernel-abi.md#os-native-profile-phase-6) for ABI details.
