# Migration from 0.2.71 to 0.2.72

0.2.72 is a semantic hard cut. The public model is now the Agent definition's
semantic identity; provider and endpoint details belong to runtime binding.

## Agent definitions

`AgentDefinition` is exported from the root SDK. Internal JSON descriptors use
`AgentDescriptor` from `@deepstrike/sdk/advanced`. `AgentSpec.inputs` was removed;
use the projection helpers when a host needs a run, context, capability,
governance or delegation view.

```ts
import { projectAgentContext } from "@deepstrike/sdk/runtime"
const context = projectAgentContext(spec)
```

`provider` is now optional at definition time. A definition with `model` but no
resolved runtime binding is accepted for composition and fails explicitly when
execution is attempted.

## Messages and providers

`ProviderMessage` remains a compatibility mirror of `ModelMessage`. New host code
should use `StoredMessage`, `RuntimeMessage`, `ModelMessage` and adapter-local
`WireMessage` according to representation. `GenerationProtocol` is the only
generation protocol type.

Prompt measurements must carry the exact request fingerprint. A stale measurement
is discarded. Unknown usage fields remain absent; they are not converted to zero.

## Package subpaths

Runtime implementation imports move to `@deepstrike/sdk/runtime`; evaluation
helpers are available from `@deepstrike/sdk/evals`. Provider adapter types remain
under `@deepstrike/sdk/providers`. Kernel and journal internals remain advanced.

## Workflow and skills

Use `WorkflowDefinition` and `WorkflowStep` from the root or workflow subpath for
agent-based workflows. Skill resources, scripts, tools, MCP servers and knowledge
references now have typed containers.
