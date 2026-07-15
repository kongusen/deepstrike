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
