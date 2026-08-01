# Canonical Kernel 性能基线

日期：2026-07-31。环境：Apple Silicon，Rust optimized `bench` profile。

运行：

```bash
cargo bench -p deepstrike-core --bench kernel_baseline
```

## 可执行门禁

Elapsed time 受本机调度影响，只作为观测数据。release gate 使用稳定的累计 allocation count/bytes；benchmark 超过任一预算会 panic 并返回非零状态。

| 场景 | 本次观测时间 | 本次分配 | 门禁预算（count / bytes） |
|---|---:|---:|---:|
| 1k 次 canonical operation 构造（configure + agent start） | 90.146 ms (90.146 µs/op) | 1,289,020 / 266,064,480 | 1,500,000 / 310,000,000 |
| 1k-message canonical start | 6.437 ms | 87,577 / 28,460,449 | 100,000 / 34,000,000 |
| canonical forced compression | 3.180 ms | 28,658 / 5,083,105 | 32,000 / 6,000,000 |
| 100-node canonical workflow start | 0.570 ms | 13,637 / 2,295,992 | 16,000 / 2,800,000 |
| 1k canonical signal deliveries | 52.313 ms (52.313 µs/op) | 2,061,745 / 262,157,154 | 2,400,000 / 310,000,000 |

所有场景都走严格 envelope decode、core record 构造和 prepare/commit；基准不再调用旧 direct-step runtime。large-context、compression、workflow 和 signal 场景均使用真实 canonical payload。signal 基线限制为 1k 次，以保持在默认 bounded-tail hard limit 内；长运行恢复另由下方 fixed-tail 门禁验证。

## 恢复成本门禁

full-journal snapshot 已删除，因此旧 snapshot encode/decode/replay 数字不再是当前 API 的有效基线。恢复门禁直接验证 logical checkpoint 成本受 tail 限制，而不是受运行总长度限制：

```bash
cargo test -p deepstrike-core long_run_restore_cost_is_bounded_by_the_tail_not_the_run -- --nocapture
```

benchmark 还断言 DAG F1 critical-path makespan 改善、F2 loop waiting rounds 为 0、F3 termination matrix 覆盖 12 个 case。allocation 包含 reallocations，bytes 是测量区间内累计分配量，不是 peak live memory。
