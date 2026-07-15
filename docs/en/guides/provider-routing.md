# Provider Routing

DeepStrike supports host-side routing across providers, protocols, and models. The kernel does not know API keys, endpoints, or provider objects. It only carries a workflow node's `model_hint` in the spawn descriptor; the SDK resolves it with `RuntimeOptions.provider_for`.

**Code entry points**:

- `python/deepstrike/providers/`
- `python/deepstrike/providers/factories.py`
- `python/deepstrike/providers/vendor_profiles.py`
- `python/deepstrike/runtime/sub_agent_orchestrator.py`
- `python/deepstrike/runtime/provider_replay.py`

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the kernel | The kernel carries only `model_hint`; it does not store API keys, endpoints, or provider objects |
| To the host | `provider_for` resolves hints into concrete model providers and protocols |
| To workflows | Different roles / nodes can route to different capability or cost tiers |
| To replay | Provider replay records protocol-shaped output so reproduction does not require live network calls |

Provider routing is the OS driver selector: the scheduler expresses the capability it needs, while the host decides which vendor, protocol, and region satisfies it.

![Provider Routing Mechanisms](/provider_routing_mechanisms.svg)

## Level 1: Default Provider

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

When there is no `model_hint`, or `provider_for` returns `None`, all runs and sub-agents use `provider`.

## Level 2: Route Workflow Nodes

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

Workflow node:

```python
WorkflowNodeSpec(
    task="perform a deep architecture review",
    role="verify",
    model_hint="deep",
)
```

If the host cannot resolve the hint, it falls back to the default provider.

## Level 3: Use Vendor Factories

Python provider factories offer a unified entry point:

```python
from deepstrike.providers.factories import deepseek, kimi, qwen, glm, minimax, gemini, ollama

p1 = deepseek(api_key=os.environ["DEEPSEEK_API_KEY"], model="deepseek-chat")
p2 = kimi(api_key=os.environ["KIMI_KEY"], region="cn", protocol="openai")
p3 = qwen(api_key=os.environ["QWEN_KEY"], region="global", protocol="anthropic")
p4 = minimax(api_key=os.environ["MINIMAX_KEY"], protocol="anthropic")
p5 = ollama(model="qwen2.5-coder")
```

`protocol` matters: different protocols have different request and replay logic.

| protocol | Typical wire |
|----------|--------------|
| `openai` | OpenAI Chat-compatible wire |
| `anthropic` | Anthropic Messages-compatible wire |

## Level 4: Region and Endpoint

`kimi` / `glm` / `qwen` support region endpoint selection:

```python
kimi(api_key=cn_key, region="cn", protocol="openai")
glm(api_key=global_key, region="global", protocol="anthropic")
```

Notes:

- region selects endpoint, not credentials
- each region usually needs that region's API key
- some combinations do not exist, such as Qwen mainland Anthropic endpoint
- explicit `base_url` overrides the region resolver

## Level 5: RuntimePolicy

Vendor profiles carry per-model runtime policy such as recommended `max_turns`. Providers can expose it with `runtime_policy()`.

```python
policy = provider.runtime_policy()
print(policy.max_turns)
```

You can still override the provider recommendation with `RuntimeOptions(max_turns=...)`.

## Level 6: Replay Compatibility

Provider replay envelopes carry protocol information. On recovery, the SDK seeds only compatible replay into a provider:

```python
from deepstrike.runtime.provider_replay import seed_provider_replay_from_events

events = await session_log.read("session-1")
seed_provider_replay_from_events(provider, events)
```

Rules:

- replay protocol matches provider descriptor → seed
- mismatch → skip the envelope
- no descriptor / replay hook → no-op

This prevents Anthropic native blocks from being replayed into OpenAI wire, or reasoning details from being sent to a provider that cannot accept them.

## Level 7: Route by Role

Common strategy:

| role / node | provider |
|-------------|----------|
| `explore` | cheap, long-context, high-throughput |
| `implement` | stable tool calling and strong code generation |
| `verify` | stronger reasoning and conservative output |
| `reduce` | no LLM, use reducer |
| `loop` | capped token budget to avoid runaway loops |

Example:

```python
def provider_for(hint: str):
    if hint == "verify":
        return providers["deep"]
    if hint == "cheap":
        return providers["fast"]
    return None
```

## Kernel / Host Boundary

| Behavior | Owner |
|----------|-------|
| carrying `model_hint` | kernel workflow descriptor |
| resolving hint to provider | SDK `provider_for` |
| API key / base_url / retry | provider instance |
| provider replay compatibility | SDK provider descriptor |
| token / turn budget | kernel scheduler + provider policy |

## Verification Entry Points

- `python/tests/test_provider_factories.py`
- `python/tests/test_provider_routing.py`
- `python/tests/test_provider_replay.py`
- `node/tests/provider-routing.test.ts`
