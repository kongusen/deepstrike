# Quick Start

Get an agent running in under five minutes.

## Installation

### Node.js

```bash
npm install @deepstrike/sdk
```

Requires Node.js 18+.

### Python

```bash
pip install deepstrike
```

Requires Python 3.10+.

### Rust

```toml
[dependencies]
deepstrike-sdk = "0.1"
tokio = { version = "1", features = ["full"] }
```

Requires Rust 1.85+.

### WASM / Browser

```bash
npm install @deepstrike/wasm
```

---

## Your first agent

### Node.js

```typescript
import { Agent, AnthropicProvider, tool } from "@deepstrike/sdk"

// 1. Define a tool
const schema = JSON.stringify({
  type: "object",
  properties: {
    x: { type: "number" },
    y: { type: "number" },
  },
  required: ["x", "y"],
})

const add = tool("add", "Add two numbers and return the sum.", schema, async ({ x, y }) => {
  return String((x as number) + (y as number))
})

// 2. Create the agent
const agent = new Agent(new AnthropicProvider(process.env.ANTHROPIC_API_KEY!), {
  maxTokens: 32_000,
})

// 3. Register the tool
agent.register(add)

// 4. Run
const result = await agent.run("What is 12 + 30?")
console.log(result.content) // "42"
```

### Python

```python
import asyncio
import os
from deepstrike import Agent, AnthropicProvider, tool

@tool
def add(x: int, y: int) -> int:
    """Add two numbers and return the sum."""
    return x + y

async def main():
    agent = Agent(
        AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"]),
        max_tokens=32_000,
    )
    agent.register(add)
    result = await agent.run("What is 12 + 30?")
    print(result.content)  # "42"

asyncio.run(main())
```

### Rust

```rust
use deepstrike_sdk::{Agent, AgentOptions, tool, providers::AnthropicProvider};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = AnthropicProvider::new(std::env::var("ANTHROPIC_API_KEY")?);
    let mut agent = Agent::new(provider, AgentOptions::new(32_000));

    agent.register(tool(
        "add",
        "Add two numbers and return the sum.",
        json!({ "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" } }, "required": ["x", "y"] }),
        |args| async move {
            let x = args["x"].as_f64().unwrap_or(0.0);
            let y = args["y"].as_f64().unwrap_or(0.0);
            Ok(format!("{}", x + y))
        },
    ));

    let result = agent.run("What is 12 + 30?").await?;
    println!("{}", result.content); // "42"
    Ok(())
}
```

---

## Streaming output

Reading stream events gives you incremental text, reasoning traces, and tool lifecycle events in real time.

### Node.js

```typescript
for await (const event of agent.runStreaming("Explain how TCP handshakes work")) {
  switch (event.type) {
    case "text_delta":
      process.stdout.write(event.delta)
      break
    case "thinking_delta":
      process.stdout.write(`\x1b[2m[thinking] ${event.delta}\x1b[0m`)
      break
    case "tool_call":
      console.log(`\n→ tool: ${event.name}`, event.arguments)
      break
    case "done":
      console.log(`\nDone. Turns: ${event.iterations}, tokens: ${event.totalTokens}`)
      break
    case "error":
      console.error("Error:", event.message)
      break
  }
}
```

### Python

```python
async for event in agent.run_streaming("Explain how TCP handshakes work"):
    if event.type == "text_delta":
        print(event.delta, end="", flush=True)
    elif event.type == "thinking_delta":
        print(f"[thinking] {event.delta}", end="", flush=True)
    elif event.type == "tool_call":
        print(f"\n→ tool: {event.name} {event.arguments}")
    elif event.type == "done":
        print(f"\nDone. Turns: {event.iterations}, tokens: {event.total_tokens}")
```

### Rust

```rust
use deepstrike_sdk::types::StreamEvent;
use futures::StreamExt;

let mut stream = agent.run_streaming("Explain TCP handshakes").await?;
while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::TextDelta { delta } => print!("{delta}"),
        StreamEvent::ThinkingDelta { delta } => print!("[thinking] {delta}"),
        StreamEvent::ToolCall { name, arguments, .. } => println!("\n→ tool: {name} {arguments}"),
        StreamEvent::Done { iterations, total_tokens, .. } => {
            println!("\nDone. Turns: {iterations}, tokens: {total_tokens}")
        }
        _ => {}
    }
}
```

---

## Multimodal input

Pass an array of content parts instead of a plain string to include images alongside text.

### Node.js

```typescript
// Image by URL
const result = await agent.run({
  role: "user",
  content: [
    { type: "text", text: "What does this chart show?" },
    { type: "image", url: "https://example.com/chart.png" },
  ],
})

// Image by base64
import { readFileSync } from "fs"
const data = readFileSync("screenshot.png").toString("base64")
const result2 = await agent.run({
  role: "user",
  content: [
    { type: "text", text: "Describe this UI." },
    { type: "image", data, mediaType: "image/png" },
  ],
})
```

### Python

```python
import base64, pathlib

# Image by URL
result = await agent.run({
    "role": "user",
    "content": [
        {"type": "text", "text": "What does this chart show?"},
        {"type": "image", "url": "https://example.com/chart.png", "detail": "high"},
    ],
})

# Image by base64
data = base64.b64encode(pathlib.Path("screenshot.png").read_bytes()).decode()
result = await agent.run({
    "role": "user",
    "content": [
        {"type": "text", "text": "Describe this UI."},
        {"type": "image", "data": data, "media_type": "image/png"},
    ],
})
```

---

## Choosing a provider

Swap the provider to change models. All providers share the same `Agent` interface.

```typescript
// Node.js — swap any of these
import { AnthropicProvider, OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider, KimiProvider } from "@deepstrike/sdk"

const provider = new AnthropicProvider(process.env.ANTHROPIC_API_KEY!, "claude-opus-4-7")
// const provider = new OpenAIProvider(process.env.OPENAI_API_KEY!, "gpt-4o")
// const provider = new QwenProvider(process.env.DASHSCOPE_API_KEY!)
// const provider = new DeepSeekProvider(process.env.DEEPSEEK_API_KEY!, "deepseek-reasoner")
// const provider = new KimiProvider(process.env.MOONSHOT_API_KEY!, "moonshot-v1-32k")
```

See [Providers](./providers.md) for full configuration options and thinking/reasoning flags.

---

## Next steps

- [Architecture](./architecture.md) — how the Rust kernel and SDK layer interact
- [Core Concepts](./core-concepts.md) — skills, memory, knowledge, harness, signals, safety
- [Providers](./providers.md) — all LLM providers and their configuration options
