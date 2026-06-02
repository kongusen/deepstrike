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
    title: Kernel-Owned Semantics
    details: Loop control, context layout, rollback, milestones, and audit behavior behind a versioned ABI.
  - icon: 🔌
    title: Host-Owned Side Effects
    details: Core is pure Rust and zero I/O. Host SDKs (Node, Python, Rust) manage tools, network, and storage.
  - icon: 🛡️
    title: Governed Execution
    details: Native capability checks, constraints, permission gates, vetoes, rate limits, and audit logs.
  - icon: ⚡
    title: Long-Run Context VM
    details: A four-slot context model that compresses history dynamically while preserving stable knowledge blocks.
  - icon: 🌐
    title: Provider Portability
    details: Anthropic, OpenAI, DeepSeek, Qwen, and local engines share a unified streaming event protocol.
  - icon: 🤝
    title: Collaboration Primitives
    details: Sub-agents, milestone gates, handoff artifacts, and verifiers built as runtime standards.
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
deepstrike-sdk = "0.2.4"
```

```bash [WASM]
npm install @deepstrike/wasm
```

:::

## Read More

- [Getting Started Guide](/getting-started/) — Install packages and run your first agent.
- [Concepts](/concepts/) — Understand the Context VM, Kernel ABI, and Governance.
- [SDK Guides](/guides/) — Full integration guides for Node.js, Python, and Rust.
- [Architecture](/architecture/) — Learn about the kernel/host split design.
- [Reference](/reference/) — Detailed Kernel ABI and lifecycle contracts.
