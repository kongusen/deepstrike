# Signals 与 Reactive Session

Signals 是 Agent OS 的 **Attention / Signal Plane**。cron、webhook、用户 interrupt 和 peer 事件不会直接改写历史，而是进入 `state_turn`，由 runner 在下一轮把它们呈现给合适的 agent。

**代码**：
- `python/deepstrike/signals/gateway.py`
- `python/deepstrike/runtime/reactive_session.py`
- Kernel：`crates/deepstrike-core/src/signals/`

---

## 在 Agent OS 中的位置

| 组件 | OS 语义 |
|------|---------|
| `SignalGateway` | 外部事件队列，负责 schedule / ingest / recipient filter |
| `state_turn` | 当前轮注意力输入，和长期 history 分离 |
| `ReactiveSession` | 多 agent 共享黑板、SignalGateway 和 RunGroup 预算 |
| `TurnPolicy` | 决定哪个 agent 对哪个事件响应 |

Signal 面解决的是“外部世界如何打断或唤醒 agent”，ReactiveSession 解决的是“多个 agent 如何在同一个治理域中协作”。

![Signals & Reactive Mechanisms](/signals_mechanisms.svg)

## 概念

```
SignalGateway
  ├── schedule(ScheduledPrompt)  # cron
  ├── ingest(RuntimeSignal)      # webhook
  ├── claim_signal(recipient?)   # 带租约消费
  ├── ack_signal / nack_signal   # 确认或重投
  └── next_signal(recipient?)    # 兼容的取出即确认接口

ReactiveSession
  ├── RunGroup        # 共享预算
  ├── EventStream     # 黑板
  ├── ReactionCheckpointStore # 幂等 plan / output
  ├── SignalGateway   # recipient 路由
  └── TurnPolicy      # 谁响应哪个事件
```

Signal 进入 kernel context 的 **signals 分区**（`state_turn`）。

---

## Level 1：定时 Prompt

```python
import time
from deepstrike import SignalGateway, ScheduledPrompt, RuntimeOptions, RuntimeRunner

gateway = SignalGateway()
gateway.schedule(ScheduledPrompt(
    goal="检查部署状态并汇报",
    run_at_ms=int(time.time() * 1000) + 60_000,
))

runner = RuntimeRunner(RuntimeOptions(
    ...,
    signal_source=gateway,
))

async for event in runner.run("开始监控"):
    ...
```

---

## Level 2：Webhook 注入

```python
from deepstrike import RuntimeSignal

# HTTP handler 中
gateway.ingest(RuntimeSignal(
    kind="external",
    payload={"event": "deploy_done", "version": "1.2.3"},
))
```

---

## Level 3：Recipient 路由与投递租约

共享 gateway 服务多个 peer 时，runner 会按 `recipient` claim，并在 kernel 接受信号后 ack；
处理异常或租约丢失时 nack。未确认的 claim 在租约过期后重新可见，因此消费者崩溃不会直接丢失信号。

手工消费时：

```python
claim = await gateway.claim_signal(recipient="analyst-1")
if claim is not None:
    try:
        await handle(claim.signal)
        await gateway.ack_signal(claim)
    except Exception:
        await gateway.nack_signal(claim)
        raise
```

旧的取出即确认接口仍保留：

```python
sig = await gateway.next_signal(recipient="analyst-1")
```

`SignalGateway` 是进程内默认实现；跨进程或重启恢复需要实现同一 `LeasedSignalSource`
契约的持久化存储，并在存储侧原子完成 claim / ack / nack。

测试：`python/tests/test_signal_addressing.py`

---

## Level 4：ReactiveSession

```python
from deepstrike import (
    ReactiveSession, ReactivePeerSpec, RunGroup,
    InMemoryGroupBudgetStore, react_by_mention,
)

group = RunGroup(id="team-1", budget_store=InMemoryGroupBudgetStore())
session = ReactiveSession(
    run_group=group,
    turn_policy=react_by_mention,
    make_runner=make_runner_fn,
    signal_gateway=SignalGateway(),
)

await session.emit(BlackboardEvent(author="user", text="@analyst 分析这个数据"))
reactions = await session.run_turns()
```

每个 persona 是一次 `runner.run(session_id=...)`，continuity 来自 SessionLog。

Stateless-friendly：`emit` 可在 HTTP handler 中调用；`resume` 从 RunGroup membership 重建 peer 集。

对可能重试的外部请求，提供稳定的 `idempotency_key`，并在新进程中复用同一个持久化
`EventStream` 与 `ReactionCheckpointStore`：

```python
reactions = await session.emit(
    "@analyst 分析这个数据",
    source="user",
    idempotency_key=request.headers["Idempotency-Key"],
)
```

checkpoint 先保存 turn-policy 选出的 persona plan，再逐个保存 output。若第二个 persona
失败，重试只补做未完成项；已完成输出会直接返回。默认 `InMemoryReactionCheckpointStore`
仅适合单进程，跨副本部署应提供实现同一原子 claim / record / complete 契约的持久化存储。

---

## 延伸阅读

- [Sub-Agent 与协作](./sub-agents-and-collaboration)
- [RunGroup 预算](../concepts/run-group-budget)
- 测试：`python/tests/test_reactive_session.py`
