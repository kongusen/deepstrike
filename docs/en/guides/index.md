# Agent Capability Guides

These guides explain how to give an Agent more useful capabilities. Start with the smallest feature that solves the current problem, then combine guides as the Agent grows.

## Choose a path

| You want your Agent to... | Read |
| --- | --- |
| answer with real data | [Tools & Integrations](./execution-plane-and-tools) → [Models & Providers](./provider-routing) |
| remember users and project facts | [Memory](./memory) → [Context & Multimodal Input](./context-engineering) |
| load specialized instructions | [Skills](./skills) → [Memory](./memory) |
| work safely | [Governance & Limits](./governance) → [Tools & Integrations](./execution-plane-and-tools) |
| delegate work | [Sub-Agents & Handoffs](./sub-agents-and-collaboration) → [Workflows](./workflow) |
| run specialists in parallel | [Workflows](./workflow) → [Structured Output & Reducers](./structured-output-and-reducers) |
| react to changing input | [Signals & Reactive Agents](./signals-and-reactive) |
| keep working over time | [Long-Running Sessions](./session-replay-and-recovery) → [Evaluation & Milestones](./milestones) |
| inspect usage and decisions | [Runtime Observability](./os-profile-and-snapshots) |

## Guide index

| Guide | Agent capability |
| --- | --- |
| [Tools & Integrations](./execution-plane-and-tools) | Typed tools, streaming tools, MCP, worktrees, sandboxes, and application-owned actions |
| [Models & Providers](./provider-routing) | Provider selection, model routing, streaming, and replay |
| [Skills](./skills) | On-demand instructions, knowledge, and focused tool access |
| [Memory](./memory) | Working memory, durable memory, recall, and session learning |
| [Context & Multimodal Input](./context-engineering) | Long context, compression, prompt caching, images, and audio |
| [Governance & Limits](./governance) | Permissions, approvals, parameter rules, quotas, budgets, and cancellation |
| [Sub-Agents & Handoffs](./sub-agents-and-collaboration) | Roles, isolated context, delegation, contracts, and handoff artifacts |
| [Workflows](./workflow) | Parallel tasks, dependencies, loops, branches, tournaments, and dynamic growth |
| [Structured Output & Reducers](./structured-output-and-reducers) | Schemas, retries, deterministic merging, and verification |
| [Signals & Reactive Agents](./signals-and-reactive) | Webhooks, schedules, host notes, peer reactions, and attention choices |
| [Long-Running Sessions](./session-replay-and-recovery) | Persistence, pause, wake, resume, replay, and recovery evidence |
| [Evaluation & Milestones](./harness-and-eval) | Quality checks, feedback, retries, milestones, and acceptance gates |
| [Runtime Observability](./os-profile-and-snapshots) | Session summaries, usage, decisions, and operational snapshots |

## Guides and reference

Guides show how to compose capabilities. The [Reference](/en/reference/) lists fields, options, and event types.
