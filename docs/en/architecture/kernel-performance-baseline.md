# Canonical Kernel Performance Baseline

Date: 2026-07-31. Profile: Apple Silicon, optimized Rust `bench` profile.

Run:

```bash
cargo bench -p deepstrike-core --bench kernel_baseline
```

## Executable Gate

Elapsed time remains characterization data because local scheduling is noisy. The release gate uses stable cumulative allocation counts/bytes; the benchmark panics and exits nonzero when any budget is exceeded.

| Scenario | Observed time | Observed allocations | Gate budget (count / bytes) |
|---|---:|---:|---:|
| 1k canonical operation constructions (configure + agent start) | 90.146 ms (90.146 µs/op) | 1,289,020 / 266,064,480 | 1,500,000 / 310,000,000 |
| 1k-message canonical start | 6.437 ms | 87,577 / 28,460,449 | 100,000 / 34,000,000 |
| canonical forced compression | 3.180 ms | 28,658 / 5,083,105 | 32,000 / 6,000,000 |
| 100-node canonical workflow start | 0.570 ms | 13,637 / 2,295,992 | 16,000 / 2,800,000 |
| 1k canonical signal deliveries | 52.313 ms (52.313 µs/op) | 2,061,745 / 262,157,154 | 2,400,000 / 310,000,000 |

Every scenario uses strict envelope decoding, core record construction, and prepare/commit; the benchmark no longer calls the retired direct-step runtime. The large-context, compression, workflow, and signal scenarios all carry real canonical payloads. The signal baseline is limited to 1k deliveries so it remains inside the default bounded-tail hard limit; the fixed-tail gate below covers long-running restore behavior.

## Restore-Cost Gate

The full-journal snapshot has been removed, so its historical encode/decode/replay numbers are no longer a valid baseline for the current API. The restore gate directly proves that logical-checkpoint cost is bounded by the tail rather than total run length:

```bash
cargo test -p deepstrike-core long_run_restore_cost_is_bounded_by_the_tail_not_the_run -- --nocapture
```

The benchmark also asserts that DAG F1 improves critical-path makespan, F2 has zero loop waiting rounds, and F3 covers all 12 termination cases. Allocation counts include reallocations; bytes are cumulative allocations during each measured section, not peak live memory.
