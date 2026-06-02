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
deepstrike-sdk = "0.2.4"
tokio = { version = "1", features = ["full"] }
```

Requires Rust 1.85+.

### WASM / Browser

```bash
npm install @deepstrike/wasm
```

---

## Your first runtime

### Node.js

```typescript
import {
  AnthropicProvider,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

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

// 2. Create the execution plane and runner
const plane = new LocalExecutionPlane().register(add)
const runner = new RuntimeRunner({
  provider: new AnthropicProvider(process.env.ANTHROPIC_API_KEY!),
  sessionLog: new InMemorySessionLog(),
  executionPlane: plane,
  maxTokens: 32_000,
})

// 3. Run
const result = await collectText(runner.run({ sessionId: "quick-start", goal: "What is 12 + 30?" }))
console.log(result) // "42"
```

### Python

```python
import asyncio
import os
from deepstrike import (
    AnthropicProvider,
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
def add(x: int, y: int) -> int:
    """Add two numbers and return the sum."""
    return x + y

async def main():
    plane = LocalExecutionPlane().register(add)
    runner = RuntimeRunner(RuntimeOptions(
        provider=AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"]),
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=32_000,
    ))
    result = await collect_text(runner.run_streaming("What is 12 + 30?"))
    print(result)  # "42"

asyncio.run(main())
```

### Rust

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    AnthropicProvider, InMemorySessionLog, LocalExecutionPlane, RegisteredTool,
    RuntimeOptions, RuntimeRunner, collect_text,
};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let provider = AnthropicProvider::new(std::env::var("ANTHROPIC_API_KEY")?);
    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "add",
        "Add two numbers and return the sum.",
        json!({ "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" } }, "required": ["x", "y"] }),
        |args| Box::pin(async move {
            let x = args["x"].as_f64().unwrap_or(0.0);
            let y = args["y"].as_f64().unwrap_or(0.0);
            Ok(format!("{}", x + y))
        }),
    ));

    let runner = RuntimeRunner::new(RuntimeOptions {
        provider: Box::new(provider),
        execution_plane: Some(Box::new(plane)),
        session_log: Some(Arc::new(InMemorySessionLog::new())),
        session_id: None,
        max_tokens: 32_000,
        max_turns: Some(10),
        timeout_ms: None,
        extensions: None,
        agent_id: None,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        signal_source: None,
        governance: None,
        on_tool_suspend: None,
    });

    let text = collect_text(
        runner.run_streaming("What is 12 + 30?", &[], None, Some("quick-start")).await?,
    )
    .await?;
    println!("{text}"); // "42"
    Ok(())
}
```

---

## Streaming output

Reading stream events gives you incremental text, reasoning traces, and tool lifecycle events in real time.

### Node.js

```typescript
for await (const event of runner.run({ sessionId: "demo", goal: "Explain how TCP handshakes work" })) {
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
async for event in runner.run_streaming("Explain how TCP handshakes work"):
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

let mut stream = runner.run_streaming("Explain TCP handshakes", &[], None, None).await?;
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
// Multimodal goals are passed via provider-native message shaping in extensions
// or by building RenderedContext turns before calling the provider directly.
// RuntimeRunner goals are plain strings; attach images in a custom provider
// or pre-seed context via system_prompt / initial_memory for simple cases.
// initial_memory → Slot 2 (system_knowledge); system_prompt → Slot 1 (system_stable).
```

### Python

```python
import base64, pathlib

# Image by URL
# See providers.md for multimodal RenderedContext patterns with RuntimeRunner.
```

---

## Choosing a provider

Swap the provider to change models. All providers share the same `LLMProvider` interface.

```typescript
// Node.js — swap any of these
import { AnthropicProvider, OpenAIProvider, QwenProvider, DeepSeekProvider, MiniMaxProvider, KimiProvider } from "@deepstrike/sdk"

const provider = new AnthropicProvider(process.env.ANTHROPIC_API_KEY!, "claude-opus-4-7")
// const provider = new OpenAIProvider(process.env.OPENAI_API_KEY!, "gpt-4o")
// const provider = new QwenProvider(process.env.DASHSCOPE_API_KEY!)
// const provider = new DeepSeekProvider(process.env.DEEPSEEK_API_KEY!, "deepseek-reasoner")
// const provider = new KimiProvider(process.env.MOONSHOT_API_KEY!, "moonshot-v1-32k")
```

See [Providers](../guides/providers.md) for full configuration options and thinking/reasoning flags.

---

## Next steps

- [Architecture](../architecture/overview.md) — how the Rust kernel and SDK layer interact
- [Core Concepts](../concepts/core-concepts.md) — skills, memory, knowledge, harness, signals, safety
- [Providers](../guides/providers.md) — all LLM providers and their configuration options
