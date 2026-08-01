# Migrating to DeepStrike 0.2.51

DeepStrike 0.2.51 is a breaking runtime release. It removes the historical v1/v2 execution paths and
uses one Canonical Kernel ABI (wire revision 3) across Rust, Node, Python and WASM.

## Before upgrading

1. Stop accepting new work on 0.2.50 hosts.
2. Let every in-flight operation finish, or cancel it explicitly.
3. Retain the 0.2.50 deployment and its durable data until no old operation must resume.
4. Upgrade every component that exchanges kernel data together: core/binding, SDK runtime and host.
5. Start new operations only after all package versions report 0.2.51.

Do not perform a rolling upgrade where a 0.2.50 host and a 0.2.51 binding share an active operation.
The new decoder intentionally rejects old or unknown revisions and does not negotiate or fall back.

## Old operations cannot resume

An unfinished operation created by the old ABI cannot be resumed by 0.2.51. There is no runtime
adapter, snapshot converter or SessionLog repair path.

- If an old operation must continue, keep that workload on 0.2.50.
- Old SessionLog entries remain valid audit evidence, but cannot create a canonical operation.
- Do not copy old full snapshots or construct `resumed_*` inputs for the new runtime.
- New recovery is authoritative only when the operation has a matching Canonical `KernelJournal`
  chain. It restores a logical checkpoint plus its bounded tail.

## Host integration changes

Replace integrations that submit old lifecycle or direct-step inputs:

| Removed assumption | 0.2.51 contract |
| --- | --- |
| `ConfigureRun` + `StartRun` | `StartOperation(RootEntry::Agent)` |
| `LoadWorkflow` + `CompleteRun` | `StartOperation(RootEntry::Workflow)` and `KernelStep.terminal` |
| direct `step`/transaction helper | canonical prepare → journal CAS append → commit |
| host-written outcome/terminal event | correlated `ResolveEffect`; terminal comes from the kernel |
| SessionLog/full snapshot recovery | `KernelJournal` + logical checkpoint + bounded tail |
| host caller/task/session identity in core input | kernel causation and kernel-issued task/attempt/launch token |
| host `SkillActivated` input | provider-result reduction owns activation |

The binding exports the current ABI revision from core. Do not hard-code a separate revision in an
SDK or select a revision per request.

## SDK behavior changes

- Node, Python and WASM retain `runAgent`/`run_agent`, `runFanout`/`run_fanout` and their existing
  `RuntimeRunner` entry points. No compatibility facade restores an old ABI operation.
- `runFanout`/`run_fanout` now preserves the canonical template's context policy: workers receive
  system-only context and the synthesis node receives full parent context.
- A rejected workflow is an explicit failure. Callers must no longer interpret an empty synthesis
  string as a successful rejection outcome.
- An unsupported effect is resolved with its original correlation ID as a non-retryable
  `ProtocolError`; custom hosts should follow the same rule.
- Consumers of WASM session events should tolerate `kernel_observation` and preserve its opaque raw
  payload when the observation kind is unknown.
- Skill content may still be loaded and pinned into knowledge, and skills may be explicitly
  deactivated. Hosts must not emit or replay `skill_activated` as a separate kernel fact.

## Durable capabilities

Production hosts must provide distinct capabilities for distinct ownership:

- `KernelJournal`: canonical records, CAS head, checkpoint install/ack and recovery;
- `SessionLog`: business/audit projection only;
- `PayloadStore`: external result bodies addressed by handle/digest;
- effect executor: provider/tool/approval/task/memory I/O with idempotency keyed by kernel IDs.

The in-memory and file implementations documented as development/test implementations do not become
cross-process transactional stores merely by upgrading. Production adapters must supply storage-level
compare-and-swap semantics.

## Package alignment check

Before deployment, verify the release metadata and runtime imports:

```bash
node scripts/sync-release-version.mjs --check
node --test scripts/release-version.test.mjs
cargo metadata --format-version=1 --no-deps
node -p "require('./node/package.json').version"
python/.venv/bin/python -c "import deepstrike; print(deepstrike.__version__)"
node -p "require('./wasm/package.json').version"
```

All reported package versions must be `0.2.51`; the kernel ABI revision remains `3` and is a separate
wire compatibility marker, not the package version.

## Rollback

Rollback is operation-scoped:

- operations started on 0.2.50 resume only on a 0.2.50 stack;
- operations started on 0.2.51 resume only on a 0.2.51 Canonical stack;
- never point one version at the other version's active journal chain.

If rollback is required, drain or cancel 0.2.51 operations first, restore the matching 0.2.50 code
and durable stores, then resume only the operations originally created by 0.2.50.
