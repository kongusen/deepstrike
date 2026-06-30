# Provider 路由

DeepStrike 支持多 provider、多协议、多模型的宿主侧路由。kernel 不知道 API key、endpoint 或模型对象；它只把 workflow node 的 `model_hint` 放进 spawn descriptor，SDK 用 `RuntimeOptions.provider_for` 解析到具体 provider。

**代码入口**：

- `python/deepstrike/providers/`
- `python/deepstrike/providers/factories.py`
- `python/deepstrike/providers/vendor_profiles.py`
- `python/deepstrike/runtime/sub_agent_orchestrator.py`
- `python/deepstrike/runtime/provider_replay.py`

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 kernel | kernel 只携带 `model_hint`，不保存 API key、endpoint 或 provider object |
| 对 host | `provider_for` 把 hint 解析成实际模型供应商和协议 |
| 对 workflow | 不同 role / node 可以路由到不同模型能力或成本层 |
| 对 replay | provider replay 记录协议相关输出，保证复现时不依赖真实网络 |

Provider 路由是 OS 的“驱动选择器”：调度层只表达需要什么能力，宿主决定用哪个厂商、协议和区域来满足。

![Provider Routing Mechanisms](/provider_routing_mechanisms.svg)

## Level 1：默认 provider

```python
from deepstrike import AnthropicProvider, RuntimeOptions, RuntimeRunner

default_provider = AnthropicProvider(
    api_key=os.environ["ANTHROPIC_API_KEY"],
    model="claude-sonnet-4-5",
)

runner = RuntimeRunner(RuntimeOptions(
    provider=default_provider,
    session_log=session_log,
))
```

没有 `model_hint` 或 `provider_for` 返回 `None` 时，所有 run 和 sub-agent 都使用 `provider`。

## Level 2：按 workflow node 路由

```python
from deepstrike import AnthropicProvider, OpenAIProvider, RuntimeOptions

providers = {
    "fast": OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"], model="gpt-4.1-mini"),
    "deep": AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"], model="claude-opus-4-1"),
}

def provider_for(hint: str):
    return providers.get(hint)

runner = RuntimeRunner(RuntimeOptions(
    provider=providers["fast"],
    provider_for=provider_for,
    session_log=session_log,
))
```

Workflow node：

```python
WorkflowNodeSpec(
    task="做深入架构评审",
    role="verify",
    model_hint="deep",
)
```

host 解析不到 hint 时会 fallback 到默认 provider。

## Level 3：选择 vendor factory

Python provider factories 为国内外厂商提供统一入口：

```python
from deepstrike.providers.factories import deepseek, kimi, qwen, glm, minimax, gemini, ollama

p1 = deepseek(api_key=os.environ["DEEPSEEK_API_KEY"], model="deepseek-chat")
p2 = kimi(api_key=os.environ["KIMI_KEY"], region="cn", protocol="openai")
p3 = qwen(api_key=os.environ["QWEN_KEY"], region="global", protocol="anthropic")
p4 = minimax(api_key=os.environ["MINIMAX_KEY"], protocol="anthropic")
p5 = ollama(model="qwen2.5-coder")
```

`protocol` 不是装饰项，不同协议有不同 request / replay 逻辑：

| protocol | 常见路径 |
|----------|----------|
| `openai` | OpenAI Chat-compatible wire |
| `anthropic` | Anthropic Messages-compatible wire |

## Level 4：region 与 endpoint

`kimi` / `glm` / `qwen` 支持 region endpoint 选择：

```python
kimi(api_key=cn_key, region="cn", protocol="openai")
glm(api_key=global_key, region="global", protocol="anthropic")
```

注意：

- region 会选择 endpoint，不会转换凭据
- 每个 region 通常需要该区域的 API key
- 某些组合不存在，例如 Qwen mainland Anthropic endpoint 不存在
- 显式 `base_url` 会覆盖 region resolver

## Level 5：RuntimePolicy

Vendor profiles 中维护 per-model runtime policy，例如推荐 `max_turns`。Provider 可以通过 `runtime_policy()` 暴露给 runner，用于和默认调度参数配合。

```python
policy = provider.runtime_policy()
print(policy.max_turns)
```

你仍可以在 `RuntimeOptions(max_turns=...)` 或 `scheduler_budget` 中显式覆盖。

## Level 6：Replay compatibility

provider replay envelope 带有协议信息。恢复 session 时，SDK 只会把兼容 replay seed 到 provider：

```python
from deepstrike.runtime.provider_replay import seed_provider_replay_from_events

events = await session_log.read("session-1")
seed_provider_replay_from_events(provider, events)
```

规则：

- replay protocol 与 provider descriptor 一致 → seed
- 不一致 → 跳过 replay envelope
- 没有 descriptor / replay hook → no-op

这避免把 Anthropic native blocks 塞进 OpenAI wire，或把 reasoning details 塞给不支持的 provider。

## Level 7：按角色路由

常见策略：

| role / node | provider |
|-------------|----------|
| `explore` | 便宜、长上下文、吞吐高 |
| `implement` | 工具调用稳定、代码能力强 |
| `verify` | 推理强、输出保守 |
| `reduce` | 无 LLM，走 reducer |
| `loop` | 可控 token cap，避免长循环烧预算 |

示例：

```python
def provider_for(hint: str):
    if hint == "verify":
        return providers["deep"]
    if hint == "cheap":
        return providers["fast"]
    return None
```

## Kernel / Host 边界

| 行为 | 所属 |
|------|------|
| `model_hint` 字段携带 | kernel workflow descriptor |
| hint 到 provider 的解析 | SDK `provider_for` |
| API key / base_url / retry | provider instance |
| provider replay compatibility | SDK provider descriptor |
| token / turn budget | kernel scheduler + provider policy |

## 验证入口

- `python/tests/test_provider_factories.py`
- `python/tests/test_provider_routing.py`
- `python/tests/test_provider_replay.py`
- `node/tests/provider-routing.test.ts`
