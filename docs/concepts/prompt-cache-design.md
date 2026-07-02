# Prompt Cache 设计

DeepStrike 的 Context 渲染不是把 messages 简单拼起来，而是把 prompt 当成一个可缓存的地址空间。核心目标是：**把每 turn 都变化的状态放到 uncached tail，把长期稳定的内容保持字节稳定**。

主要实现入口：

- `crates/deepstrike-core/src/context/renderer.rs`
- `crates/deepstrike-core/src/context/manager.rs`
- `crates/deepstrike-core/src/context/compression.rs`
- `crates/deepstrike-core/src/mm/handle.rs`
- `node/src/types.ts` / `python/deepstrike/runtime/runner.py` 的 turn metrics

## RenderedContext 的四个槽位

`RenderedContext` 是 provider 调用前的结构化 prompt：

```text
system_stable       Identity / stable system prompt
system_knowledge    Skill definitions / initial_memory / host-pinned durable knowledge
turns               History (incl. runtime memory-tool hits & prefetch); cacheable prefix
state_turn          TASK STATE + signals + recency footer; volatile tail
```

渲染形状：

```text
[ system_stable ]       ← stable system block
[ system_knowledge ]    ← knowledge block
[ turns[0..frozen] ]    ← deep cache breakpoint when available
[ turns[frozen..] ]     ← hot history tail
[ state_turn ]          ← rebuilt every render, not part of turns
```

`system_text = system_stable + system_knowledge` 只服务于 OpenAI 这类单 system slot provider；Anthropic 可以分别给 system block 和 message history 放 cache breakpoint。

## 为什么 state_turn 不在 turns 里

`state_turn` 包含：

- `[TASK STATE]`：goal、criteria、plan、blocked_on、compression log
- signals：rollback、interrupt、外部事件
- salience footer：最近真实工具动作、下一步、最新 directive
- `Proceed.` anchor

这些内容每 turn 都可能变化。如果把它放进 `turns`，就会让 cacheable message prefix 每轮漂移。现在它作为 volatile tail，由 provider adapter 自己决定放在 history 后面或前面。

## PrefixFingerprint

每次 render 都可计算一个 cache prefix 指纹：

```rust
pub struct PrefixFingerprint {
    pub system_stable_hash: u64,
    pub system_knowledge_hash: u64,
    pub turn_hashes: Vec<u64>,
}
```

它只 hash provider wire 上会影响 cache 的内容：

| 包含 | 排除 |
|------|------|
| `system_stable` | `state_turn` |
| `system_knowledge` | `token_count` 元数据 |
| 每个 history turn 的 role / content / tool_calls | runtime-only 统计 |

`extends(prev)` 的语义是：当前 prefix 是否只是上一次的字节稳定扩展。只要中间某个 turn 被原地改写，`common_turn_prefix(prev)` 就会变短，说明 cache 命中会从该点开始失效。

## frozen_prefix_len

`ContextManager` 在 compaction / renewal 后维护 `frozen_history_len`。渲染时会把它换算成 `RenderedContext.frozen_prefix_len`：

```text
history before compaction boundary  → frozen prefix
history after boundary              → hot tail
```

当存在非空 frozen region 且后面还有 hot tail 时，provider 可以在该边界放 deep cache breakpoint。这样 heavy tool turn 不会因为最近 block 太多而错过更深的缓存前缀。

pre-compaction 或没有明确 frozen region 时，`frozen_prefix_len = None`，provider 回退到 rolling breakpoint 策略。

## HandleTable 与 read-time projection

大工具结果进入 history 时，`ContextManager.push_history` 会为每个 `ToolResult` 创建 handle：

```rust
Handle {
    kind: HandleKind::ToolResult,
    residency: Residency::Resident,
    tokens,
    source: Some(call_id),
}
```

handle 的 `Residency` 决定 render 时如何投影：

| Residency | 行为 |
|-----------|------|
| `Resident` | full content 仍在工作上下文中 |
| `Collapsed` | 原文保留在 history，但 render 成 preview |
| `SpooledOut` | SDK 持久化完整结果，context 留 preview / ref |
| `PagedOut` | 内容归档到 memory tier |

`Collapsed` 是非破坏性的：stored history 不改，rendered copy 变短。这让旧工具结果能在压力下退出 prompt，同时保留可恢复数据。

## 压缩层与 cache 成本

压缩的执行器返回：

```rust
pub struct CompressResult {
    pub tokens_saved: u32,
    pub summary: Option<String>,
    pub archived: Vec<Message>,
    pub prefix_invalidated_at: Option<usize>,
}
```

`prefix_invalidated_at` 表示最早被改写或删除的 history index：

| 值 | 含义 |
|----|------|
| `None` | prefix-safe；没有改写 cacheable prefix |
| `Some(0)` | 从最早消息开始破坏 prefix |
| `Some(n)` | 第 n 个 history message 之后的 cache 失效 |

Pipeline 会取所有 stage 的最小 invalidation index。`ContextManager` 只有在 prefix 真的被破坏时才重新锚定 `frozen_history_len`。

## 何时压缩

压力评估来自 `PressureMonitor`：

- raw `rho()`：用于决定是否进入压缩层级
- `effective_rho()`：估算路径下扣除 non-resident handle tokens，用于 paging-aware 判断
- provider usage 可覆盖估算 token count

当前压缩层级包括：

| 层 | 行为 | cache 影响 |
|----|------|------------|
| Snip | 截断 oversized text message | 可能改写中间 turn |
| MicroCompact | 摘要 / excerpt 大 tool result | 通常较晚、相对 prefix-safe |
| ContextCollapse | drop oldest 到 target | prefix break |
| AutoCompact | 只保留最近 K turns | prefix break |
| TimeDecayMicro | idle 后微压缩 | 与 pressure 层独立 |

## SDK 观测

Python:

```python
RuntimeOptions(
    ...,
    on_turn_metrics=lambda m: print(m.cache_read_tokens),
)
```

可观测字段包括：

- `input_tokens`
- `cache_read_tokens`
- `cache_creation_tokens`
- `cache_read_tokens_by_slot`
- `tools_exposed`
- `tools_called`

Anthropic adapter 还会对 slot 做归因；OpenAI-family 自动缓存时不一定有等价 slot 数据。

## 实践建议

1. **保持 `system_prompt` 稳定**：system drift 会让整个 prefix 失效。
2. **按需加载 Skill 正文**：避免频繁 churn `system_knowledge`。
3. **使用 `allowed_tool_ids` 静态 profile**：工具 schema 稳定更利于 cache。
4. **不要原地改写早期 history**：append 比 rewrite 更 cache-friendly。
5. **大工具结果优先走 handle / spool / collapse**：不要把超大结果长期常驻 prompt。
6. **把动态状态放进 task_state / signals**：让它进入 `state_turn`，不要污染 cacheable history。

## 延伸阅读

- [Context 工程](../guides/context-engineering)
- [执行模型](../architecture/execution-model)
- [Kernel ABI](../architecture/kernel-abi)
