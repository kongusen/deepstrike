# Migrating to `@deepstrike/sdk` v0.2.30

v0.2.30 streamlines the public API from ~90 flat root exports to ~30 root + six subpaths, and removes the
kernel-boundary plumbing that was never meant to be called by applications. This is a **breaking** change
with no compatibility shims. Most migrations are a one-line import-path change.

## 1. Moved to subpaths

Update the import path; the symbols themselves are unchanged.

| Symbol(s) | Old | New |
|---|---|---|
| `endpointProfiles`, `modelProfiles`, `getModelProfile`, `CircuitBreaker`, `OpenAIChatProvider` | `@deepstrike/sdk` | `@deepstrike/sdk/providers` |
| `SubAgentOrchestrator`, `defaultSubAgentOrchestrator`, `spawnStandalone`, `builtinReducers`, `resolveReducer`, `FileWorkflowStore`, `ContractBuilder`, `VerificationContract`, `HandoffBus`, `CreatorVerifierMode`, `OrchestrationMode`, `submitWorkflowNodesTool`, `startWorkflowTool`, `generateAndFilter`, `verifyRules`, `genEval`, agent/milestone types | `@deepstrike/sdk` | `@deepstrike/sdk/workflow` |
| `WorktreeExecutionPlane`, `GitWorktreeManager`, `FilteredExecutionPlane`, `ProcessSandboxPlane`, `McpProxyPlane`, `RemoteVpcPlane`, `NullArchiveStore`, `FileArchiveStore`, credential vaults | `@deepstrike/sdk` | `@deepstrike/sdk/planes` |
| `WorkingMemory`, `InMemoryDreamStore`, `DreamStore`, `MemoryRecord`, `MemoryRecall`, `MemoryQuery`, `MemoryScope`, `SessionData`, `CurationResult`, `KnowledgeSource`, … | `@deepstrike/sdk` | `@deepstrike/sdk/memory` |
| `AttemptLoop`, `RuntimeAttemptBody`, judge/carry policies, `judge`, `Criterion`, `Verdict` | `@deepstrike/sdk` | `@deepstrike/sdk/harness` |
| `osProfile`, `assertNativeProfile`, `KernelPrimitivesDashboard`, `rebuildOsSnapshotFromSessionEvents`, `ReplayProvider`, `extractRecordedMessages`, `PermissionManager`, `PermissionMode`, `SignalGateway`, `ScheduledPrompt`, replay-validator utils, scheduler/quota/policy types | `@deepstrike/sdk` | `@deepstrike/sdk/os` |

```diff
- import { DeepSeekProvider, WorkingMemory } from "@deepstrike/sdk"
+ import { AttemptLoop, RuntimeAttemptBody } from "@deepstrike/sdk/harness"
+ import { DeepSeekProvider } from "@deepstrike/sdk/providers"
+ import { WorkingMemory } from "@deepstrike/sdk/memory"
```

## 2. Renamed

| Old | New |
|---|---|
| `OpenAIChatProvider` (root) | `OpenAIProvider` (root) — same class; or import `OpenAIChatProvider` from `@deepstrike/sdk/providers` |
| `fanoutSynthesize(spec…)` | `runFanout({ provider, tasks, synthesize })` (root) |
| `new DeepSeekProvider(apiKey, model)` | `deepseek({ apiKey, model })` from `@deepstrike/sdk/providers` |
| `new DeepSeekAnthropicProvider(apiKey, model)` | `deepseek({ apiKey, model, protocol: "anthropic" })` |
| `new KimiProvider` / `new QwenProvider` / `new GLMProvider` | `kimi({…})` / `qwen({…})` / `glm({…})` |
| `new MiniMaxAnthropicProvider` / `new MiniMaxOpenAIProvider` | `minimax({…})` (Anthropic default) / `minimax({…, protocol: "openai" })` |
| `new GeminiProvider` / `new OllamaProvider` | `gemini({…})` / `ollama({…})` |

The dual `<Backend>Provider` / `<Backend>AnthropicProvider` class families are replaced by **one factory
function per backend**, with a `protocol` option selecting the wire where a backend supports both. The
classes still exist internally (and remain importable from their source files for advanced subclassing),
but are no longer part of the public `@deepstrike/sdk/providers` surface.

## 3. Provider constructors are options-objects

The positional `(apiKey, model, retry, baseURL)` hole is gone. The base providers accept an options object
(the legacy positional form still works on the base classes for now, but is deprecated):

```diff
- new OpenAIProvider(apiKey, "mimo-v2.5-pro", undefined, "https://host/v1")
+ new OpenAIProvider({ apiKey, model: "mimo-v2.5-pro", baseURL: "https://host/v1" })
```

Or let the catalog pick the protocol/endpoint: `createProvider({ model, apiKey, baseURL })`.

## 4. Removed (internalized — no public replacement)

These crossed the kernel boundary or were low-level building blocks the high-level APIs now own. If you
were calling them directly, switch to `runWorkflow` / `AttemptLoop` / `Governance`:

`workflowSpecToKernel`, `workflowNodeSpecToKernel`, `submitWorkflowToKernel`, `submitWorkflowNodesToKernel`,
`agentRunSpecToKernel`, `subAgentResultToKernel`, `milestoneCheckResultToKernel`, `agentIdentitySub`,
`governancePolicyToKernelEvent`, `kernelObservationToSessionEvent`, `categoryForKind`, `KERNEL_ROLE_MAP`,
`normalizeToolCall`, `OpenAIChatAdapter`, `OpenAIResponsesAdapter`, `loopInstruction`, `classifyInstruction`,
`judgeGoal`, `extractLoopContinue`, `extractClassifyBranch`, `extractJudgeWinner`, `buildEvalMessages`,
`parseVerdict`, `verdictOutputSchema`, `fanoutSynthesize`.
