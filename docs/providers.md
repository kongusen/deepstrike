# Providers

All providers implement the `LLMProvider` interface and plug into `RuntimeRunner` via `RuntimeOptions.provider`. They share `RetryConfig` (exponential backoff) and `CircuitBreaker` (automatic failure isolation).

---

## Provider matrix

| Provider | API endpoint | Default model | Thinking / Reasoning | Images | SDK availability |
|----------|-------------|---------------|----------------------|--------|-----------------|
| `AnthropicProvider` | `api.anthropic.com` | `claude-sonnet-4-6` | `ThinkingDelta` via `enable_thinking` | URL + base64 | Node / Python / Rust / WASM |
| `OpenAIChatProvider` / `OpenAIProvider` | OpenAI Chat Completions | `gpt-4o` | — | URL + base64 (data-URI) | Node / Python / Rust / WASM |
| `OpenAIResponsesProvider` | OpenAI Responses | `gpt-4.1` | Native `previous_response_id` continuation | URL + base64 (data-URI) | Node |
| `QwenProvider` | DashScope `dashscope.aliyuncs.com` | `qwen3.6-plus` | `ThinkingDelta` via `enableThinking` | URL | Node / Python / Rust / WASM |
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
for await (const event of runner.run({
  sessionId: "demo",
  goal,
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

## OpenAIChatProvider / OpenAIProvider

Compatible with any OpenAI-compatible Chat Completions API (OpenAI, Azure OpenAI, local gateways). Pass a custom `baseUrl` to redirect traffic. `OpenAIProvider` remains a compatibility alias for `OpenAIChatProvider`.

### Node.js

```typescript
import { OpenAIChatProvider } from "@deepstrike/sdk"

const provider = new OpenAIChatProvider(
  process.env.OPENAI_API_KEY!,
  "gpt-4o",                         // optional; default: "gpt-4o"
  { maxRetries: 3, baseDelay: 1000 },
  "https://my-gateway.example.com/v1",  // optional custom base URL
)
```

Use `OpenAIResponsesProvider` for OpenAI's Responses API and native run continuation:

```typescript
import { OpenAIResponsesProvider } from "@deepstrike/sdk"

const provider = new OpenAIResponsesProvider(
  process.env.OPENAI_API_KEY!,
  "gpt-5-mini",
)
```

The Node catalog selects the protocol from model profiles:

```typescript
import { createProvider } from "@deepstrike/sdk"

const provider = createProvider({
  model: "openai/gpt-5-mini",
  apiKey: process.env.OPENAI_API_KEY!,
})
```

For future models or third-party gateways, keep the provider family explicit and pass the custom model/base URL directly:

```typescript
const provider = createProvider({
  provider: "openai",
  model: "gpt-next-custom",
  apiKey: process.env.GATEWAY_API_KEY!,
  baseURL: "https://gateway.example.com/v1",
})
```

Custom models can also use a provider-prefixed name such as `qwen/qwen-next-custom` or an explicit endpoint such as `endpoint: "glm.openai"`. Known model profiles still select their catalog default endpoint unless you intentionally override `endpoint`.

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

Backed by Alibaba's DashScope. Supported chat profiles start at Qwen 3.5 and include the Qwen 3.7 preview, Qwen 3.6, and Qwen 3.5 families. Embedding profiles include `text-embedding-v4`, `text-embedding-v3`, `qwen3-vl-embedding`, and `qwen2.5-vl-embedding`.

The Node model catalog also includes embedding profiles for OpenAI (`text-embedding-3-large`, `text-embedding-3-small`, `text-embedding-ada-002`), Gemini (`gemini-embedding-2`, `gemini-embedding-001`), GLM (`embedding-3`, `embedding-2`), and BAAI BGE (`bge-m3`, BGE v1.5 English/Chinese sizes, `bge-code-v1`, and BGE-VL v1.5). Embedding endpoints are tracked separately from chat endpoints so `createProvider` does not accidentally construct a chat adapter for a vectorization model. Qwen text embeddings use the OpenAI-compatible embeddings endpoint; Qwen VL embeddings use DashScope's multimodal embeddings endpoint. BGE profiles are metadata for self-hosted or Hugging Face/FlagEmbedding deployments rather than a managed API adapter.

### Thinking mode

Pass `enableThinking: true` in `extensions` to activate Qwen's extended thinking. `thinkingBudget` (token limit) is optional.

### Node.js

```typescript
import { QwenProvider } from "@deepstrike/sdk"

const provider = new QwenProvider(process.env.DASHSCOPE_API_KEY!, "qwen3.6-plus")

for await (const event of runner.run({
  sessionId: "demo",
  goal,
  extensions: { enableThinking: true, thinkingBudget: 4096 },
})) {
  if (event.type === "thinking_delta") process.stdout.write(`[thinking] ${event.delta}`)
  if (event.type === "text_delta") process.stdout.write(event.delta)
}
```

### Python

```python
from deepstrike import QwenProvider

provider = QwenProvider(api_key=os.environ["DASHSCOPE_API_KEY"], model="qwen3.6-plus")

async for event in runner.run_streaming(goal, extensions={"enable_thinking": True, "thinking_budget": 4096}):
    if event.type == "thinking_delta":
        print(f"[thinking] {event.delta}", end="", flush=True)
    elif event.type == "text_delta":
        print(event.delta, end="", flush=True)
```

### Rust

```rust
use deepstrike_sdk::providers::qwen;

let provider = qwen(std::env::var("DASHSCOPE_API_KEY")?, Some("qwen3.6-plus"));
```

---

## DeepSeekProvider

Supports both chat models (`deepseek-chat`) and reasoning models (`deepseek-reasoner`, `deepseek-r1`). Reasoning models emit a `reasoning_content` field in stream deltas, exposed as `ThinkingDelta` when `exposeReasoning` is set. Tools are automatically stripped for reasoning models.

### Node.js

```typescript
import { DeepSeekProvider } from "@deepstrike/sdk"

const provider = new DeepSeekProvider(process.env.DEEPSEEK_API_KEY!, "deepseek-reasoner")

for await (const event of runner.run({
  sessionId: "demo",
  goal,
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

async for event in runner.run_streaming(goal, extensions={"exposeReasoning": True}):
    ...
```

### Rust

```rust
use deepstrike_sdk::providers::deepseek;
use serde_json::json;

let provider = deepseek(api_key, Some("deepseek-reasoner"));
// pass expose_reasoning in extensions when calling runner.run_streaming
```

---

## MiniMaxProvider

Supports MiniMax chat models and the MiniMax-M1 reasoning model. M1 behaves like a reasoner: tools are stripped automatically, and `reasoning_content` is streamed as `ThinkingDelta` when `exposeReasoning` is set.

### Node.js

```typescript
import { MiniMaxProvider } from "@deepstrike/sdk"

const provider = new MiniMaxProvider(process.env.MINIMAX_API_KEY!, "MiniMax-M1")

for await (const event of runner.run({
  sessionId: "demo",
  goal,
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

async for event in runner.run_streaming(goal, extensions={"exposeReasoning": True}):
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
// Build multimodal turns on RenderedContext, then call provider.stream() directly,
// or encode image URLs in the goal / system_prompt for simple cases.
// RuntimeRunner goals are strings; see provider-specific ContentPart mapping below.
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
