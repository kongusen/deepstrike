# Kernel ABI v2 Performance Baseline

Date: 2026-07-15. Profile: Apple Silicon, optimized Rust `bench` profile.

Run with:

```bash
cargo bench -p deepstrike-core --bench kernel_baseline
```

| Scenario | Time | Allocations | Allocated bytes |
|---|---:|---:|---:|
| 10k kernel steps | 14.505 ms (1.450 µs/op) | 285,016 | 34,756,685 |
| 1k-message render, 100 iterations | 5.077 ms (50.769 µs/op) | 100,900 | 41,505,800 |
| forced compression | 0.226 ms | 4,052 | 1,504,608 |
| 100-node workflow submit | 0.223 ms | 4,705 | 653,475 |
| 10k signal deliveries | 29.360 ms (2.936 µs/op) | 620,018 | 57,193,012 |
| 10k-input snapshot encode | 4.456 ms | 30,031 | 14,287,767 |
| 10k-input snapshot decode + deterministic replay | 40.499 ms | 1,220,283 | 129,141,254 |
| encoded snapshot size | 3,556,595 bytes | — | — |

Allocation counts include reallocations and report cumulative allocated bytes during each measured section, not peak live memory. These numbers are characterization data, not release gates. They justify keeping snapshot history explicitly bounded and SDK-configurable; optimize only after a repeatable regression appears in this harness.

## Recovery hot-path result

After replacing JSON-tree fingerprints with canonical JSON bytes, serializing snapshots from a
borrowed view, and suppressing duplicate journal recording during deterministic restore:

| Scenario | Before | After | Allocation change |
|---|---:|---:|---:|
| 10k kernel steps | 0.899 µs/op, 285,016 allocs / 34,756,685 bytes | 0.701 µs/op, 185,016 allocs / 25,878,051 bytes | -35.1% allocs, -25.5% bytes |
| 10k signal deliveries | 2.340 µs/op, 620,018 allocs / 57,193,012 bytes | 2.049 µs/op, 370,018 allocs / 45,646,446 bytes | -40.3% allocs, -20.2% bytes |
| 10k-input snapshot encode | 5.656 ms, 30,031 allocs / 14,287,767 bytes | 3.061 ms, 17 allocs / 8,388,487 bytes | -99.9% allocs, -41.3% bytes |
| 10k-input snapshot decode + replay | 48.042 ms, 1,220,283 allocs / 129,141,254 bytes | 20.025 ms, 390,168 allocs / 56,408,963 bytes | -68.0% allocs, -56.3% bytes |

Elapsed time is retained as characterization data because it is sensitive to local scheduling;
allocation counts and bytes are the primary regression signal for this slice. Snapshot wire bytes
are measured after a warm run. The byte-resource metadata adds 98 snapshot bytes; the encoded
10k-input checkpoint is 3,556,693 bytes.
