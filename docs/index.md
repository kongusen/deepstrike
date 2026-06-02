---
layout: home

hero:
  name: DeepStrike
  text: Agent OS Microkernel
  tagline: Replayable state, governed tools, and cross-language runtime SDKs for AI agents.
  image:
    src: /banner.png
    alt: DeepStrike Logo
  actions:
    - theme: brand
      text: Quick Start
      link: /getting-started/quick-start
    - theme: alt
      text: GitHub Repository
      link: https://github.com/kongusen/deepstrike

features:
  - icon: 🧠
    title: Agent OS Microkernel
    details: Syscall trap, scheduler lifecycle, and memory management — kernel decides when and whether; SDKs execute I/O.
  - icon: 🔌
    title: Host-Owned Side Effects
    details: Pure Rust core, zero I/O. Node, Python, Rust, and WASM SDKs own providers, tools, spool, and long-term memory.
  - icon: 🛡️
    title: Governed by Default
    details: Declarative governance and in-kernel signal routing on every run — deny, ask_user, rate limits, and audit events.
  - icon: ⚡
    title: Long-Run Sessions
    details: Four-slot Context VM, Layer-1 large-result spool, semantic page-out, and a unified compression funnel.
  - icon: 💾
    title: Memory Syscalls
    details: Kernel-validated writeMemory / queryMemory outside the tool loop, with session-log and OS snapshot counters.
  - icon: 🤝
    title: Collaboration Primitives
    details: Sub-agents, milestone gates, process table, handoff artifacts, and verifiers as runtime standards.
---

## Supported Ecosystem

Choose your runtime and get started in seconds:

::: code-group

```bash [Node.js / TS]
npm install @deepstrike/sdk
```

```bash [Python]
pip install deepstrike
```

```toml [Rust]
# Cargo.toml
[dependencies]
deepstrike-sdk = "0.2.5"
```

```bash [WASM]
npm install @deepstrike/wasm
```

:::

## Read More

- [Agent OS](./concepts/agent-os) — What 0.2.5 enables: kernel mediation, spool, memory syscalls, and observability.
- [Getting Started Guide](/getting-started/) — Install packages and run your first agent.
- [Concepts](/concepts/) — Context VM, memory, governance, signals, and collaboration.
- [SDK Guides](/guides/) — Full integration guides for Node.js, Python, and Rust.
- [Architecture](/architecture/) — Kernel/host split and runtime loop design.
- [Reference](/reference/) — Kernel ABI, lifecycle contracts, and OS parity matrix.
