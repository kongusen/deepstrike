# Getting Started

DeepStrike helps you build Agents that can use tools, remember facts, collaborate with other Agents, and keep working across sessions.

## Recommended path

1. [Install an SDK](./installation) for Python, Node.js, Rust, or WASM.
2. Run [Hello Agent](./hello-agent) with one tool.
3. Choose between [`run_agent`], [`run_fanout`], and `RuntimeRunner` in [Choosing an API](./run-agent-vs-runner).
4. Connect a [Provider](./providers).
5. Add the capability your Agent needs from the [Agent capability guides](/en/guides/).

## Choose an entry point

| API | Best for |
| --- | --- |
| `run_agent()` | One goal, optional tools, and a final text result. |
| `run_fanout()` | Several focused tasks followed by one synthesis step. |
| `RuntimeRunner` | Streaming, sessions, memory, signals, governance, workflows, and custom execution. |

## Learn by building

The [Research Brief Studio examples](https://github.com/kongusen/deepstrike/tree/main/example) provide an eight-level path from a sourced Q&A Agent to a reactive editorial team.
