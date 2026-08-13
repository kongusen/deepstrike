# Providers and Streaming Events

DeepStrike lets an Agent use different model providers without changing its tools, memory, or workflow code. Python implementations live in `python/deepstrike/providers/`.

## Supported Providers

| Provider | Class |
|----------|-------|
| Anthropic | `AnthropicProvider` |
| OpenAI Chat | `OpenAIProvider` |
| OpenAI Responses | `OpenAIResponsesProvider` |
| DeepSeek | `DeepSeekProvider` |
| Qwen | `QwenProvider` |
| Kimi | `KimiProvider` |
| Ollama | `OllamaProvider` |
| Gemini | See providers directory |
| GLM / MiniMax | See providers directory |

## Basic Usage

```python
from deepstrike import AnthropicProvider, OpenAIProvider

provider = AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"])
# provider = OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"])
```

## Streaming Event Types

`runner.run()` yields `StreamEvent` subclasses:

| Event | Meaning |
|-------|---------|
| `TextDelta` | Incremental model text |
| `ThinkingDelta` | Reasoning chain (when provider supports it) |
| `ToolCallEvent` | Model initiates a tool call |
| `ToolResultEvent` | Tool execution result |
| `ToolSuspendEvent` | Tool suspended (requires `on_tool_suspend`) |
| `PermissionRequestEvent` | Governance `ask_user` |
| `DoneEvent` | Run completed |
| `ErrorEvent` | Error |

## How providers affect an Agent

The same Agent context is mapped to each provider's native request format:

- **Anthropic**: dual `system_stable` + `system_knowledge` blocks + cache breakpoint
- **OpenAI**: merged into a single `system_text`

See [Prompt Cache Design](/en/concepts/prompt-cache-design).

## Multi-Model Routing

Workflow nodes can set `model_hint`; the host resolves it via `RuntimeOptions.provider_for`:

```python
def provider_for(hint: str):
    if hint == "fast":
        return OpenAIProvider(model="gpt-4o-mini", ...)
    return default_provider

RuntimeOptions(provider=default_provider, provider_for=provider_for)
```

## Replay for Testing

```python
from deepstrike import ReplayProvider, ProviderReplayOpts
# Fixed responses for CI — see python/tests/test_provider_replay.py
```

## Further Reading

- [Context Engineering](/en/guides/context-engineering)
- Node SDK: `node/README.md`
