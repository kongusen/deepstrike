# 0.2.69 Framework Verifiable Runtime Foundation

0.2.69 的核心是 Framework Verifiable Runtime Foundation。它为已经落盘的执行历史提供一个
与存储无关的框架对象 `VerifiableOperation`。文件系统、数据库、对象存储和浏览器内存都可以
先由宿主适配器组装 `EvidenceBundle`，再调用同一组框架操作。

```text
VerifiableOperation
  ├─ inspect(strict)
  ├─ verify(VerifyOptions)
  ├─ replay(ReplayOptions)
  └─ prepare_fork(at_step, strict)
```

框架只接收证据字节，不打开路径、不调用 Provider、不写 Journal、不修改 Checkpoint，也不
产生恢复权威。`deepstrike` CLI 只是文件读取和结果呈现适配器；Rust、Node、Python、WASM
SDK 复用同一语义边界。

SDK 的 native adapter 只做字节数组到 `verifiable_operation_json` 的编码，验证结果仍由 Rust
core 产生。没有 native binding 时，宿主也可以实现同一个 `VerifiableRuntimeAdapter` 契约，
但不能在 SDK 层复制 C1–C8 规则。

CLI 适配器提供：

```text
deepstrike inspect <operation> [evidence options]
deepstrike verify <operation> [evidence options] [--require-complete]
deepstrike replay <operation> [evidence options] [--at <step>]
deepstrike fork <operation> [evidence options] --at <step> --output <path>
```

JSON 输出固定为 `verifiable-report/v2`，退出码固定为 `0` 通过、`1` 证实矛盾、`2` 证据
不足或检查不可用、`64` 参数错误。`deepstrike inspect|verify|replay|fork` 是唯一命令入口。

`replay` 只使用已记录的证据，绝不会调用真实 Provider。`fork` 只写入包含父操作、边界
步骤和父记录 digest 的只读 manifest，不写 Kernel Journal、不修改 Checkpoint，也不成为
恢复权威。Evolution 对象和内容寻址 ArtifactVersion 延后到 0.2.70+。

性能基线使用 `cargo bench -p deepstrike-core --bench verifiable_baseline` 捕获，基线测量的是
固定记录链上的 inspect 查询与 C1–C8 验证开销。
