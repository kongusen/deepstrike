# Providers

All providers implement the `LLMProvider` interface and can be dropped into any `Agent`. They share `RetryConfig` (exponential backoff) and `CircuitBreaker` (automatic failure isolation).

---

## Provider matrix

| Provider | API endpoint | Default model | Thinking / Reasoning | Images | SDK availability |
|----------|-------------|---------------|----------------------|--------|-----------------|
| `AnthropicProvider` | `api.anthropic.com` | `claude-sonnet-4-6` | `ThinkingDelta` via `enable_thinking` | URL + base64 | Node / Python / Rust / WASM |
| `OpenAIProvider` | `api.openai.com` | `gpt-4o` | — | URL + base64 (data-URI) | Node / Python / Rust / WASM |
| `QwenProvider` | DashScope `dashscope.aliyuncs.com` | `qwen-max` | `ThinkingDelta` via `enableThinking` | URL | Node / Python / Rust / WASM |
| `DeepSeekProvider` | `api.deepseek.com` | `deepseek-chat` | `ThinkingDelta` via `exposeReasoning` (reasoner models) | — | Node / Python / Rust / WASM |
| `MiniMaxProvider` | `api.minimax.chat` | `MiniMax-Text-01` | `ThinkingDelta` via `exposeReasoning` (M1 models) | — | Node / Python / Rust / WASM |
| `KimiProvider` | `api.moonshot.cn` | `moonshot-v1-8k` | — | URL (vision models) | Node / Python / Rust / WASM |
| `OllamaProvider` | `localhost:11434` | `llama3` | — | base64 array | Python / Rust |

---

## AnthropicProvider

Supports all Claude models. Extended thinking emits `ThinkingDelta` events alongside regular `TextDelta` events.

### Node.js

```typescript
import { AnthropicProvider } from "@deepstrike/sdk"

const provider = new AnthropicProvider(
  process.env.ANTHROPIC_API_KEY!,
  "claude-opus-4-7",            // optional; default: "claude-sonnet-4-6"
  { maxRetries: 3, baseDelay: 1000 },  // optional RetryConfig
)
```

**With extended thinking:**

```typescript
for await (const event of agent.runStreaming(goal, {
  extensions: { enable_thinking: true, thinking_budget_tokens: 8000 },
})) {
  if (event.type === "thinking_delta") process.stdout.write(`[thinking] ${event.delta}`)
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

### Python

```python
from deepstrike import AnthropicProvider, RetryConfig

provider = AnthropicProvider(
    api_key=os.environ["ANTHROPIC_API_KEY"],
    model="claude-opus-4-7",
    retry_config=RetryConfig(max_retries=3, base_delay=1.0),
)
```

### Rust

```rust
use deepstrike_sdk::providers::AnthropicProvider;

let provider = AnthropicProvider::new(std::env::var("ANTHROPIC_API_KEY")?);
// or with model override:
let provider = AnthropicProvider::with_model(api_key, "claude-opus-4-7");
```

---

## OpenAIProvider

Compatible with any OpenAI-compatible API (OpenAI, Azure OpenAI, local gateways). Pass a custom `baseUrl` to redirect traffic.

### Node.js

```typescript
import { OpenAIProvider } from "@deepstrike/sdk"

const provider = new OpenAIProvider(
  process.env.OPENAI_API_KEY!,
  "gpt-4o",                         // optional; default: "gpt-4o"
  { maxRetries: 3, baseDelay: 1000 },
  "https://my-gateway.example.com/v1",  // optional custom base URL
)
```

### Python

```python
from deepstrike import OpenAIProvider

provider = OpenAIProvider(
    api_key=os.environ["OPENAI_API_KEY"],
    model="gpt-4o",
    base_url="https://my-gateway.example.com/v1",  # optional
)
```

### Rust

```rust
use deepstrike_sdk::providers::OpenAIProvider;

let provider = OpenAIProvider::new(api_key);
// or with base URL:
let provider = OpenAIProvider::with_base_url(api_key, "gpt-4o", "https://my-gateway/v1");
```

---

## QwenProvider

Backed by Alibaba's DashScope. Models include the `qwen-max` / `qwen-plus` / `qwen-turbo` family and extended-thinking models such as `qwen3-235b-a22b`.

### Thinking mode

Pass `enableThinking: true` in `extensions` to activate Qwen's extended thinking. `thinkingBudget` (token limit) is optional.

### Node.js

```typescript
import { QwenProvider } from "@deepstrike/sdk"

const provider = new QwenProvider(process.env.DASHSCOPE_API_KEY!, "qwen3-235b-a22b")

for await (const event of agent.runStreaming(goal, {
  extensions: { enableThinking: true, thinkingBudget: 4096 },
})) {
  if (event.type === "thinking_delta") process.stdout.write(`[thinking] ${event.delta}`)
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

### Python

```python
from deepstrike import QwenProvider

provider = QwenProvider(api_key=os.environ["DASHSCOPE_API_KEY"], model="qwen3-235b-a22b")

async for event in agent.run_streaming(goal, extensions={"enable_thinking": True, "thinking_budget": 4096}):
    if event.type == "thinking_delta":
        print(f"[thinking] {event.delta}", end="", flush=True)
    elif event.type == "text_delta":
        print(event.delta, end="", flush=True)
```

### Rust

```rust
use deepstrike_sdk::providers::qwen;

let provider = qwen(std::env::var("DASHSCOPE_API_KEY")?, Some("qwen3-235b-a22b"));
```

---

## DeepSeekProvider

Supports both chat models (`deepseek-chat`) and reasoning models (`deepseek-reasoner`, `deepseek-r1`). Reasoning models emit a `reasoning_content` field in stream deltas, exposed as `ThinkingDelta` when `exposeReasoning` is set. Tools are automatically stripped for reasoning models.

### Node.js

```typescript
import { DeepSeekProvider } from "@deepstrike/sdk"

const provider = new DeepSeekProvider(process.env.DEEPSEEK_API_KEY!, "deepseek-reasoner")

for await (const event of agent.runStreaming(goal, {
  extensions: { exposeReasoning: true },
})) {
  if (event.type === "thinking_delta") process.stdout.write(`[reasoning] ${event.delta}`)
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

### Python

```python
from deepstrike import DeepSeekProvider

provider = DeepSeekProvider(api_key=os.environ["DEEPSEEK_API_KEY"], model="deepseek-reasoner")

async for event in agent.run_streaming(goal, extensions={"exposeReasoning": True}):
    ...
```

### Rust

```rust
use deepstrike_sdk::providers::deepseek;
use serde_json::json;

let provider = deepseek(api_key, Some("deepseek-reasoner"));
// pass expose_reasoning in extensions when calling agent.run_streaming
```

---

## MiniMaxProvider

Supports MiniMax chat models and the MiniMax-M1 reasoning model. M1 behaves like a reasoner: tools are stripped automatically, and `reasoning_content` is streamed as `ThinkingDelta` when `exposeReasoning` is set.

### Node.js

```typescript
import { MiniMaxProvider } from "@deepstrike/sdk"

const provider = new MiniMaxProvider(process.env.MINIMAX_API_KEY!, "MiniMax-M1")

for await (const event of agent.runStreaming(goal, {
  extensions: { exposeReasoning: true },
})) {
  if (event.type === "thinking_delta") process.stdout.write(`[reasoning] ${event.delta}`)
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

### Python

```python
from deepstrike import MiniMaxProvider

provider = MiniMaxProvider(api_key=os.environ["MINIMAX_API_KEY"], model="MiniMax-M1")

async for event in agent.run_streaming(goal, extensions={"exposeReasoning": True}):
    ...
```

### Rust

```rust
use deepstrike_sdk::providers::minimax;

let provider = minimax(api_key, Some("MiniMax-M1"));
```

---

## KimiProvider

Backed by Moonshot AI. Fully OpenAI-compatible — no special extensions needed. Models: `moonshot-v1-8k`, `moonshot-v1-32k`, `moonshot-v1-128k`. Vision is available on `-vision-preview` variants (URL-only images).

### Node.js

```typescript
import { KimiProvider } from "@deepstrike/sdk"

const provider = new KimiProvider(process.env.MOONSHOT_API_KEY!, "moonshot-v1-32k")
```

### Python

```python
from deepstrike import KimiProvider

provider = KimiProvider(api_key=os.environ["MOONSHOT_API_KEY"], model="moonshot-v1-32k")
```

### Rust

```rust
use deepstrike_sdk::providers::kimi;

let provider = kimi(api_key, Some("moonshot-v1-32k"));
```

---

## OllamaProvider

Runs against a local Ollama instance (default `http://localhost:11434`). No API key required. Supports vision models via the `images` array in Ollama's API.

Available on Python and Rust SDKs only (not WASM/browser).

### Python

```python
from deepstrike import OllamaProvider

provider = OllamaProvider(model="llama3.2-vision")
# or custom endpoint:
provider = OllamaProvider(model="mistral", base_url="http://192.168.1.10:11434")
```

### Rust

```rust
use deepstrike_sdk::providers::ollama;

let provider = ollama("llama3.2-vision", None); // None → default base URL
```

---

## RetryConfig and CircuitBreaker

All providers accept a `RetryConfig` that controls exponential backoff on transient errors.

```python
# Python
from deepstrike import RetryConfig

provider = AnthropicProvider(
    api_key="...",
    retry_config=RetryConfig(max_retries=5, base_delay=0.5),
)
# delay series: 0.5s, 1.0s, 2.0s, 4.0s, 8.0s
```

```typescript
// Node.js — third constructor argument
const provider = new AnthropicProvider(apiKey, "claude-sonnet-4-6", {
  maxRetries: 5,
  baseDelay: 500,
})
```

`CircuitBreaker` is built into every provider. After 5 consecutive failures it opens the circuit and throws immediately rather than retrying. It auto-resets after 60 seconds. You don't need to configure it manually.

---

## Multimodal support

Image inputs are handled automatically by the provider layer. Pass a `content` array in the message:

```typescript
// Node.js — works with Anthropic, OpenAI, Qwen (vision models), Kimi (vision models)
const result = await agent.run({
  role: "user",
  content: [
    { type: "text", text: "What's in this image?" },
    { type: "image", url: "https://example.com/chart.png" },
    // or base64:
    // { type: "image", data: base64String, mediaType: "image/jpeg" }
  ],
})
```

The provider serialises `ContentPart` to the correct wire format automatically:

| Provider | URL image | Base64 image |
|----------|-----------|-------------|
| Anthropic | `source: {type:"url"}` | `source: {type:"base64", media_type, data}` |
| OpenAI-compat | `image_url: {url}` | `image_url: {url: "data:{mt};base64,{data}"}` |
| Ollama | not supported | `images: [base64string]` |

---

## Custom / OpenAI-compatible endpoint

Use `OpenAIProvider` with a custom `baseUrl` for any OpenAI-compatible gateway:

```typescript
// Node.js
const provider = new OpenAIProvider(apiKey, "my-model", undefined, "https://my-llm-proxy/v1")
```

```python
# Python
provider = OpenAIProvider(api_key=apiKey, model="my-model", base_url="https://my-llm-proxy/v1")
```
