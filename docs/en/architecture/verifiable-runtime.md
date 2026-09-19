# Framework Verifiable Runtime Foundation

0.2.70 continues the storage-neutral `VerifiableOperation` foundation. A filesystem, database, object-store,
or browser adapter assembles an `EvidenceBundle` and calls the same framework operations:

```text
VerifiableOperation
  ├─ inspect(strict)
  ├─ verify(VerifyOptions)
  ├─ replay(ReplayOptions)
  └─ prepare_fork(at_step, strict)
```

The framework accepts evidence bytes only. It does not open paths, call a provider, append to the
Journal, mutate a Checkpoint, or create recovery authority. The `deepstrike` CLI is a filesystem
and presentation adapter; Rust, Node, Python, and WASM SDKs reuse the same semantic boundary.

Each SDK native adapter only encodes byte arrays for `verifiableOperationJson`; Rust core produces
the validation result. A host may implement the same `VerifiableRuntimeAdapter` contract for another
store, but C1–C8 rules must not be copied into an SDK.

The CLI adapter exposes:

```text
deepstrike inspect <operation> [evidence options]
deepstrike verify <operation> [evidence options] [--require-complete]
deepstrike replay <operation> [evidence options] [--at <step>]
deepstrike fork <operation> [evidence options] --at <step> --output <path>
```

JSON output is frozen as `verifiable-report/v2`. Exit codes are `0` pass, `1` proven contradiction,
`2` insufficient evidence or unavailable check, and `64` usage error. `deepstrike inspect|verify|replay|fork`
is the only command entry point.

`replay` uses recorded evidence only and never invokes a live provider. `fork` writes only a
read-only manifest containing the parent operation, boundary step, and parent digest. It does not
write the Kernel Journal, mutate a Checkpoint, or become recovery authority. Evolution objects,
content-addressed ArtifactVersion, and promotion validation are defined by the [Evolution Runtime](./evolution-runtime),
with the artifact-set binding fixed at operation genesis.

The baseline command is `cargo bench -p deepstrike-core --bench verifiable_baseline`; it measures
inspect and C1–C8 validation cost on a fixed record chain.
