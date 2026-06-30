# Provider 与流式事件

SDK 通过 `LLMProvider` 抽象各厂商 API。Python 实现在 `python/deepstrike/providers/`。

## 支持的 Provider

| Provider | 类 |
|----------|-----|
| Anthropic | `AnthropicProvider` |
| OpenAI Chat | `OpenAIProvider` |
| OpenAI Responses | `OpenAIResponsesProvider` |
| DeepSeek | `DeepSeekProvider` |
| Qwen | `QwenProvider` |
| Kimi | `KimiProvider` |
| Ollama | `OllamaProvider` |
| Gemini | 见 providers 目录 |
| GLM / MiniMax | 见 providers 目录 |

## 基本用法

```python
from deepstrike import AnthropicProvider, OpenAIProvider

provider = AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])
# provider = OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"])
```

## 流式事件类型

`runner.run()` 产出 `StreamEvent` 子类：

| 事件 | 含义 |
|------|------|
| `TextDelta` | 模型文本增量 |
| `ThinkingDelta` | 推理链（若 provider 支持） |
| `ToolCallEvent` | 模型发起工具调用 |
| `ToolResultEvent` | 工具执行结果 |
| `ToolSuspendEvent` | 工具挂起（需 `on_tool_suspend`） |
| `PermissionRequestEvent` | 治理 ask_user |
| `DoneEvent` | run 完成 |
| `ErrorEvent` | 错误 |

## RenderedContext 与 Provider

Kernel 渲染四槽位上下文（`RenderedContext`），Provider 负责映射到各 API 格式：

- **Anthropic**：`system_stable` + `system_knowledge` 双 block + cache breakpoint
- **OpenAI**：合并为单一 `system_text`

见 [Prompt Cache 设计](../concepts/prompt-cache-design)。

## 多模型路由

工作流节点可设 `model_hint`，宿主通过 `RuntimeOptions.provider_for` 解析：

```python
def provider_for(hint: str):
    if hint == "fast":
        return OpenAIProvider(model="gpt-4o-mini", ...)
    return default_provider

RuntimeOptions(provider=default_provider, provider_for=provider_for)
```

## 测试用 Replay

```python
from deepstrike import ReplayProvider, ProviderReplayOpts
# 固定响应，用于 CI — 见 python/tests/test_provider_replay.py
```

## 延伸阅读

- [Context 工程](../guides/context-engineering)
- Node SDK：`node/README.md`
