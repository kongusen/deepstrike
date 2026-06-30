# Getting Started

Welcome to DeepStrike — an agent runtime framework built for production.

## Recommended Reading Order

1. [Installation](./installation) — Python / Node / Rust
2. [Hello Agent](./hello-agent) — Your first runnable agent
3. [Choosing an API](./run-agent-vs-runner) — `run_agent` vs `RuntimeRunner` vs `run_fanout`
4. [Providers](./providers) — Connecting to LLMs

## Three API Levels

| API | Use case |
|-----|----------|
| `run_agent()` | Single prompt → text result (covers ~90% of scenarios) |
| `run_fanout()` | Parallel N tasks + synthesis |
| `RuntimeRunner` | Streaming events, signals, memory, governance, workflows |

Source: `python/deepstrike/runtime/facade.py`
