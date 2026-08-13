# Spec: Remove All Legacy Compatibility

## Objective

DeepStrike 0.2.60 removes every first-party legacy compatibility path and every V1/V2-style
mechanism identity from production code. Git history, changelogs, migration notes, and negative
rejection fixtures remain the historical record; the runtime and public SDK surfaces support one
unnumbered canonical mechanism.

This is intentionally breaking. Old checkpoints, replay envelopes, aliases, constructors, default
semantics, and compatibility fallbacks are not migrated at runtime. First-party canonical payloads
do not negotiate or dispatch by version. Where stale data can cross a durable or public boundary,
it must fail closed as an invalid canonical shape.

## Scope

Remove:

- Kernel checkpoint v1 lifting and old `Content` JSON carriers.
- First-party runtime version axes including `abi_version`, `checkpoint_version`, `schema_version`,
  and context-policy `version` fields.
- V1/V2 suffixes and prefixes on canonical types, functions, constants, fixtures, and documentation.
- Legacy durable-content and durable-tool-result decoders and migration helpers.
- Provider replay protocol inference for envelopes without an explicit protocol.
- Legacy scheduler projections, waits, default behavior, and text-sniffed workflow fallbacks.
- Deprecated Node and WASM aliases and provider wrapper classes.
- Python compatibility-only provider shims and old constructor forms where they exist.
- Legacy collaboration role mappings and documentation for nonexistent runner fallbacks.
- Legacy benchmark input importers.
- Compatibility tests that assert old inputs still work.

Retain:

- Git history, `CHANGELOG.md`, released-version migration documents, and ADR history.
- Negative fixtures and tests that prove unsupported old inputs are rejected deterministically.
- Current protocol interoperability code, including OpenAI-compatible and Anthropic-compatible
  providers and `node/src/compat/` ecosystem adapters. “Compatible” does not mean “legacy.”
- Package release versions, third-party API paths such as `/v1`, third-party model names such as
  `moonshot-v1-*`, and externally owned protocol identifiers.

## One-Mechanism Rule

The repository must describe and implement the current design directly, never as the latest member
of an in-process version family.

- Canonical types use names such as `ContextPolicy`, `TransitionState`, `SchedulerState`,
  `SyscallState`, and `ContextVmState`, without `V1`, `V2`, `Current`, or `Latest` qualifiers.
- Canonical builders use names such as `contextPolicy` / `context_policy` and
  `normalizeContextPolicy` / `normalize_context_policy`.
- First-party canonical envelopes, checkpoints, durable content, replay records, and context policy
  carry no runtime version discriminator.
- Decoders implement one strict shape. There is no revision enum, dispatch table, negotiation,
  migration ladder, or inference path.
- Tests refer to `canonical`, `accepted`, or `rejected` fixtures instead of calling the active
  format V1/V2.
- Comments and architecture documents explain the mechanism itself, not the sequence of historical
  milestones that produced it.

Package semantic versions remain normal release metadata. Third-party strings containing versions
remain untouched because DeepStrike does not own those contracts.

## Canonical Behavior After Removal

- Only the canonical checkpoint shape is readable and writable, with no checkpoint revision field.
- Durable content always uses the canonical block representation, with no schema-version field.
- Provider replay always declares its protocol; missing or mismatched protocols are rejected or
  skipped according to the current explicit replay contract, never inferred from payload shape.
- The kernel envelope uses one strict ABI shape with no ABI revision field or revision dispatch.
- Context policy uses one strict shape with no version member.
- Scheduler tasks use only durable `WaitSet` state and canonical kernel roles.
- Workflow continuation uses kernel decisions only; model-text JSON sniffing is not a fallback.
- Optional capability and exposure fields use the current strict semantics; omission does not
  resurrect an older errs-open behavior.
- Public SDKs expose factories and canonical types only. Removed names do not remain as aliases.

Fail-closed Rust style:

```rust
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KernelCheckpoint {
    operation_id: OperationId,
    logical_state: LogicalKernelState,
}
```

Equivalent TypeScript and Python boundaries must reject unsupported shapes before they reach the
kernel or provider adapters. Former version fields are unknown fields and therefore fail closed.

## Project Structure

- `crates/deepstrike-core/src/` — canonical kernel, checkpoint, scheduler, and durable types.
- `node/src/`, `python/deepstrike/`, `wasm/src/`, `rust/src/` — public SDK surfaces and runtime
  bindings.
- `node/tests/`, `python/tests/`, `wasm/tests/`, `crates/**/src/**` test modules — behavior and
  rejection coverage.
- `benchmark/` — benchmark formats and import paths.
- `docs/`, `README*`, `MIGRATION-*`, `CHANGELOG.md` — current docs plus immutable historical record.

## Commands

Repository scans:

```sh
git grep -n -i legacy -- 'crates/*/src/**' 'node/src/**' 'python/deepstrike/**' 'rust/src/**' 'wasm/src/**' 'benchmark/**'
git grep -n -E '@deprecated|useLegacyRunners|use_legacy_runners' -- 'node/src/**' 'python/deepstrike/**' 'wasm/src/**'
git grep -n -E '\b(V1|V2|v1|v2)\b|[A-Za-z_](V1|V2|v1|v2)[A-Za-z_]*' -- 'crates/*/src/**' 'node/src/**' 'python/deepstrike/**' 'rust/src/**' 'wasm/src/**'
git grep -n -E '\b(abi_version|checkpoint_version|schema_version)\b' -- 'crates/*/src/**' 'node/src/**' 'python/deepstrike/**' 'rust/src/**' 'wasm/src/**'
```

Build and test:

```sh
cargo test --workspace
npm --prefix node run build
npm --prefix node test -- --runInBand
python -m pytest python/tests
npm --prefix wasm run build
npm --prefix wasm test -- --runInBand
npm --prefix benchmark test
```

## Code Style

- Prefer deletion over replacement shims.
- Use one canonical type and one canonical execution path.
- Name the mechanism directly; never name the active design `V1`, `V2`, `Current`, or `Latest`.
- Reject unsupported durable input at the boundary with a specific error.
- Do not retain renamed symbols through aliases, re-exports, or deprecated subclasses.
- Keep protocol interoperability data-driven; do not confuse current wire compatibility with
  historical API compatibility.
- Keep commits scoped to one compatibility cluster and its tests.

## Testing Strategy

1. Add or update a rejection test before removing each durable compatibility decoder.
2. Update compile-time and API-surface tests before deleting each deprecated public symbol.
3. Run focused tests after every compatibility cluster.
4. Run the full Rust, Node, Python, and WASM suites at phase checkpoints.
5. Finish with source scans proving there are no production `legacy` markers, deprecated aliases,
   obsolete compatibility options, first-party V1/V2 mechanism names, runtime version axes, or
   old-format acceptance tests.

Historical documents and negative fixture names are exempt from the final word scan. Current
production protocol names such as `OpenAI-compatible` and `Anthropic-compatible` are also exempt.
Third-party URLs, model identifiers, and protocol literals are exempt from V1/V2 scans.

## Boundaries

Always:

- Preserve current canonical behavior and cross-SDK parity.
- Convert old-format acceptance tests into explicit rejection tests where the boundary persists.
- Keep the repository buildable after each implementation slice.
- Preserve unrelated working-tree changes and integrate around them.

Ask first:

- Any compatibility exception that would survive into 0.2.60.
- Any deletion of historical documentation or Git history.
- Any dependency or CI workflow change not required by the removal.

Never:

- Silently reinterpret an old durable payload as the current format.
- Hide an obsolete API behind a new alias or undocumented fallback.
- Add a new runtime format version, `Current*` alias, or multi-version decoder to replace a deleted
  V1/V2 mechanism.
- Delete current OpenAI/Anthropic protocol interoperability code merely because its name contains
  “compatible.”
- Overwrite or discard pre-existing uncommitted work.

## Success Criteria

- Production source contains no first-party legacy execution path or legacy-named public symbol.
- Production source contains no `@deprecated` declaration retained for compatibility.
- Production source contains no first-party V1/V2 mechanism names or runtime format-version axes.
- Old checkpoint and durable-content/tool-result shapes fail deterministically as non-canonical.
- Replay envelopes without explicit protocol metadata are never inferred by shape.
- Removed SDK classes, aliases, constructors, and options are absent from exports and type stubs.
- Node, Python, WASM, Rust, and kernel expose the same canonical behavior.
- All build and test commands above pass.
- Release notes identify 0.2.60 as a breaking removal and list the new minimum persisted formats and
  public entry points.

## Delivery Constraints

The current working tree contains unrelated, uncommitted durable-wait work in:

- `crates/deepstrike-core/src/runtime/kernel/wire/driver.rs`
- `crates/deepstrike-core/src/scheduler/tcb.rs`
- `crates/deepstrike-core/src/scheduler/wait_index.rs`

Legacy wait removal was integrated with those edits rather than replacing or reverting them. The
user approved the 0.2.60 hard cutover, including rejection of 0.2.53 persisted data.
