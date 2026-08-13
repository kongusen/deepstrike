---
layout: home

hero:
  name: DeepStrike
  text: A local Agent Process Runtime
  tagline: Durable process trees, authority, budgets, scheduling, communication, and recovery for Agents that keep working.
  image:
    src: /banner.png
    alt: DeepStrike
  actions:
    - theme: brand
      text: Explore Agent Runtime
      link: /en/architecture/agent-process-runtime
    - theme: alt
      text: Quick Start
      link: /en/getting-started/hello-agent
    - theme: alt
      text: Tutorial Curriculum
      link: https://github.com/kongusen/deepstrike/tree/main/example
    - theme: alt
      text: GitHub Wiki
      link: https://github.com/kongusen/deepstrike/wiki

features:
  - icon: ⚙️
    title: Agent Process Runtime
    details: Treat root Agents, child Agents, and workflow nodes as local runtime tasks with lineage, lifecycle, and supervision.
  - icon: 🧰
    title: Use real tools
    details: Add typed tools, streaming tools, MCP servers, files, sandboxes, worktrees, and application-owned integrations.
  - icon: 🧠
    title: Remember and learn
    details: Combine working memory, durable memory, skills, and knowledge sources without turning every prompt into a transcript dump.
  - icon: 🤝
    title: Delegate and collaborate
    details: Run focused child Agents, hand off artifacts, fan out research, verify results, and build reactive peer teams.
  - icon: ⏳
    title: Durable wait and recovery
    details: Wait for effects, children, approvals, signals, or timers, then continue from checkpoints and replay after a restart.
  - icon: ✅
    title: Authority, budgets, and determinism
    details: Attenuate child capabilities, allocate hierarchical resource budgets, and schedule through one predictable runnable set.
---

## Start With The Agent You Need

::: code-group

```bash [Python]
pip install deepstrike
```

```bash [Node.js / TypeScript]
npm install @deepstrike/sdk
```

```toml [Rust]
[dependencies]
deepstrike-sdk = "0.2"
```

```bash [WASM]
npm install @deepstrike/wasm
```

:::

| You want to build... | Start here |
| --- | --- |
| Understand the Agent runtime model | [Agent Process Runtime](/en/architecture/agent-process-runtime) |
| A single tool-using Agent | [Hello Agent](/en/getting-started/hello-agent) |
| An Agent with memory or skills | [Agent capabilities](/en/guides/) |
| A multi-Agent workflow | [Dynamic workflows](/en/guides/workflow) |
| A recoverable long-running Agent | [Sessions and recovery](/en/guides/session-replay-and-recovery) |
| A peer-based Agent team | [Sub-Agents and collaboration](/en/guides/sub-agents-and-collaboration) |
| The complete API surface | [Reference](/en/reference/) |

## Learn By Building

The [Research Brief Studio curriculum](https://github.com/kongusen/deepstrike/tree/main/example) grows one product across eight runnable levels:

1. Ask questions with sources and tools.
2. Recall durable facts across sessions.
3. Load skills and task-specific knowledge.
4. React to signals and changing input.
5. Apply policies, approvals, and resource limits.
6. Pace a long-running loop and wake it later.
7. Fan out to specialists and verify structured results.
8. Bring peers together around a shared editorial task.

Every level has TypeScript and Python examples, plus a `--dry-run` path for validating wiring without provider credentials.

## Documentation Channels

- **VitePress**: run `npm run docs:dev` locally; switch between Chinese and English from the top navigation.
- **GitHub Wiki**: generated from `docs/` by CI; see [Wiki sync](https://github.com/kongusen/deepstrike/blob/main/docs/wiki/README.md).
