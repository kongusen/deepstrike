---
layout: home

hero:
  name: DeepStrike
  text: Agent Runtime Kernel
  tagline: Replayable state, governed tools, cross-language SDKs — production-grade dynamic workflows.
  image:
    src: /banner.png
    alt: DeepStrike
  actions:
    - theme: brand
      text: Quick Start
      link: /en/getting-started/hello-agent
    - theme: alt
      text: What is Agent OS
      link: /en/architecture/agent-os
    - theme: alt
      text: Architecture
      link: /en/architecture/
    - theme: alt
      text: GitHub Wiki
      link: https://github.com/kongusen/deepstrike/wiki

features:
  - icon: 🧠
    title: Kernel + SDK Split
    details: Pure Rust kernel; Python / Node / WASM SDKs own I/O. SDK feeds events; kernel returns actions.
  - icon: 🕸️
    title: Dynamic Workflows
    details: Declarative DAG + runtime append + Loop / Classify / Tournament control-flow nodes.
  - icon: 📦
    title: Context Engineering
    details: Four-slot rendering, pressure compression, handle paging, prompt-cache aware.
  - icon: 🛡️
    title: In-Kernel Governance
    details: Syscall traps, quotas, rate limits, denied tool results — not post-hoc SDK filtering.
  - icon: 💾
    title: Memory Syscalls
    details: Kernel-validated writeMemory / queryMemory + DreamStore + idle consolidation pipeline.
  - icon: 🤝
    title: Multi-Agent Collaboration
    details: Sub-agent isolation, ReactiveSession, contracts, handoff.
---

## Supported SDKs

::: code-group

```bash [Python]
pip install deepstrike
```

```bash [Node.js / TS]
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

## Reading Paths

| You are… | Start here |
|----------|------------|
| New user | [Hello Agent](/en/getting-started/hello-agent) |
| Integrator | [Choosing an API](/en/getting-started/run-agent-vs-runner) → [Guides](/en/guides/) |
| Architect | [Kernel / SDK Split](/en/architecture/overview) |
| Wiki reader | [GitHub Wiki](https://github.com/kongusen/deepstrike/wiki) (synced from `docs/`) |

## Documentation Channels

- **VitePress** (this site): `npm run docs:dev` locally; GitHub Pages on push to `main`
- **GitHub Wiki**: synced from `docs/` via CI — see [Wiki sync docs](https://github.com/kongusen/deepstrike/blob/main/docs/wiki/README.md) (`docs/wiki/README.md`)

Switch language via the **简体中文 / English** toggle in the top navigation.
