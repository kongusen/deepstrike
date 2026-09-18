# Model Choice & Provider Routing

DeepStrike lets each Agent or workflow node use the model that fits its job. Route by provider, protocol, capability, latency, or cost without changing the Agent's tools or instructions.

**Code entry points**:

- `python/deepstrike/providers/`
- `python/deepstrike/providers/factories.py`
- `python/deepstrike/providers/vendor_profiles.py`
- `python/deepstrike/runtime/sub_agent_orchestrator.py`
- `python/deepstrike/runtime/provider_replay.py`

## What routing can do for an Agent

| Need | Routing behavior |
| --- | --- |
| Different model tiers | Send fast exploratory work to one model and verification to another. |
| Vendor portability | Keep Agent code stable while changing provider adapters. |
| Per-node choice | Route workflow roles or nodes independently. |
| Testable runs | Replay recorded provider responses without live network calls. |

Provider routing keeps model choice an application decision while the Agent remains focused on its task.

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

When neither `endpoint` nor `protocol` is explicit, 0.2.61 uses these defaults:

| vendor | Default protocol |
|--------|------------------|
| DeepSeek / Kimi / Qwen / GLM | OpenAI Chat-compatible |
| MiniMax | Anthropic Messages-compatible |
| Anthropic / OpenAI / Gemini / Ollama | Their official protocol |

Resolution order is `explicit endpoint > explicit protocol > vendor default`. Explicit
Anthropic-compatible routes remain available.

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
- mismatched replay without tool calls → skip the envelope
- mismatched tool-call replay → raise `provider_replay_protocol_mismatch` before dispatch
- no descriptor / replay hook → no-op

The diagnostic contains only provider and protocol identifiers, never thinking text, tool names, or
arguments. When resuming an old Session after a default-route change, pin its previous endpoint or
protocol explicitly. The SDK does not guess a conversion from Anthropic native blocks to OpenAI reasoning.

## Anthropic-Compatible Textual Tool-Call Safety

When tools are exposed, built-in Anthropic-compatible endpoints and unknown custom Anthropic endpoints
reject the exact DSML tool-call sentinel by default. Candidate control text is never emitted as a
`text_delta` or final answer. The retryable error is classified as `protocol / textual_tool_call`, and
the existing Kernel recovery policy decides whether to retry.

Official `anthropic.messages` leaves this detection off by default. A caller can override the per-call
extension with `textualToolCallPolicy="off" | "reject"`. This host-only field is never sent on the
provider wire. Rejection detects and discards only: it does not parse arguments, synthesize tool calls,
or execute tools. Text recovery is a separate future capability.

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

## Token Measurement and Capability Semantics

Token accounting has three value families whose semantics are not interchangeable:

| Field | Sole meaning |
|------|----------|
| `ContextTokenEngine` / `TokenMeasurement` | runtime budgeting uses kernel recomputation or a host measurement table; provider input/output counts remain usage/attempt evidence |
| `ProviderUsage.inputTokens` | the provider-visible full prompt token count (the authority for billing and observation) |
| `PromptMeasurement.inputTokens` | a pre-send measurement or estimate of the same wire plan; it feeds pre-send budgeting only and never impersonates billing facts |

`native token counting` is endpoint evidence, not protocol inheritance. Endpoints with executable native counting today:

| Endpoint | Requirement | Method |
|----------|------|------|
| `anthropic.messages` | `@anthropic-ai/sdk ^0.99` / `anthropic>=0.40` | `messages.countTokens` |
| `gemini.google` | `@google/generative-ai ^0.24` / `google-genai>=1.0` | `models.countTokens` |
| `openai.responses` | `openai ^7.5.0` / `openai>=2.6` | `responses.inputTokens.count`; Chat Completions does not inherit it |

Semantics:

- a registry `supported` at runtime means the provider instance really exposes a callable count method; an official endpoint without a wired adapter stays unavailable
- a custom `base_url` does not inherit official evidence by default; an explicitly selected endpoint counts as vouching for that endpoint family (capability kept, runtime failures degrade to heuristic)
- preflight reuses the same request plan as create/stream; failures or timeouts degrade to a heuristic estimate, and only native/local-exact results may hard-reject before send
- postflight observed usage is the authority: the runner feeds observed input tokens back as a `postflight`-sourced measurement record journaled under `prompt_measured`; replay reuses the observed fact by request fingerprint without counting again

## Runtime and Application Responsibilities

| Behavior | Owner |
|----------|-------|
| carrying `model_hint` | workflow descriptor |
| resolving hint to provider | SDK `provider_for` |
| API key / base_url / retry | provider instance |
| provider replay compatibility | SDK provider descriptor |
| token / turn budget | runtime scheduler + provider policy |

## Verification Entry Points

- `python/tests/test_provider_factories.py`
- `python/tests/test_provider_routing.py`
- `python/tests/test_provider_replay.py`
- `python/tests/test_anthropic_protocol_adapter.py`
- `node/tests/model-registry.test.ts`
- `node/tests/provider-fallback-replay.test.ts`
- `node/tests/anthropic-textual-tool-call.test.ts`
