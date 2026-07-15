---
# code_refs: validated by scripts/check-docs-drift.mjs against python __all__ — symbols must exist.
code_refs:
  python: [RuntimeRunner, RuntimeOptions, AnthropicProvider, OpenAIProvider, OpenAIResponsesProvider, LLMProvider, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent, tool, LocalExecutionPlane, WorkflowSpec, WorkflowNodeSpec, Governance, AgentPool, ReactiveSession, RuntimeSignal, run_agent, run_fanout, DeepSeekProvider, QwenProvider, KimiProvider, OllamaProvider, RenderedContext, streaming_tool, read_file, WorkingMemory, DreamStore, MemoryRecord, MemoryPolicy, fanout_synthesize, generate_and_filter, verify_rules, workflow_spec_to_kernel, ResourceQuota, AgentRunSpec, HandoffBus, AttemptLoop, AttemptJudge, SignalGateway, ScheduledPrompt]
---

# Python API Index

Public exports for `from deepstrike import ...`, defined in `python/deepstrike/__init__.py`.

## Entry points

| Symbol | Description |
|--------|-------------|
| `run_agent` | Single-agent shortcut |
| `run_fanout` | Parallel + synthesize |
| `RuntimeRunner` | Full runner |
| `RuntimeOptions` | Runner configuration |

## Provider

| Symbol | Description |
|--------|-------------|
| `AnthropicProvider` | Anthropic API |
| `OpenAIProvider` | OpenAI Chat |
| `OpenAIResponsesProvider` | OpenAI Responses |
| `DeepSeekProvider` | DeepSeek |
| `QwenProvider` | Qwen (Tongyi) |
| `KimiProvider` | Kimi |
| `OllamaProvider` | Ollama |
| `LLMProvider` | Base protocol |
| `RenderedContext` | Rendered context |

## Streaming events

| Symbol | Description |
|--------|-------------|
| `TextDelta` | Text delta |
| `ToolCallEvent` | Tool call |
| `ToolResultEvent` | Tool result |
| `DoneEvent` | Completion |
| `ErrorEvent` | Error |
| `PermissionRequestEvent` | Governance ask_user |

## Tools

| Symbol | Description |
|--------|-------------|
| `tool` | Decorator to register tools |
| `streaming_tool` | Streaming tool |
| `read_file` | Built-in read file |
| `LocalExecutionPlane` | Local tool execution |

## Memory

| Symbol | Description |
|--------|-------------|
| `WorkingMemory` | In-process scratch |
| `DreamStore` | Protocol (implement as needed) |
| `InMemoryDreamStore` | In-memory implementation |
| `MemoryRecord` | Memory record |
| `MemoryPolicy` | Memory policy |

## Workflow

| Symbol | Description |
|--------|-------------|
| `WorkflowSpec` | DAG spec |
| `WorkflowNodeSpec` | Node spec |
| `fanout_synthesize` | Template |
| `generate_and_filter` | Template |
| `verify_rules` | Template |
| `workflow_spec_to_kernel` | Convert to kernel JSON |

## Governance

| Symbol | Description |
|--------|-------------|
| `Governance` | Governance wrapper |
| `GovernancePolicy` | Declarative policy |
| `ResourceQuota` | Resource quota |

## Collaboration

| Symbol | Description |
|--------|-------------|
| `AgentPool` | Role pool |
| `AgentRunSpec` | Sub-agent spec |
| `HandoffBus` | Handoff bus |
| `AttemptLoop` | Retry loop (body x judge x carry) |
| `ReactiveSession` | Multi-peer reactive |

## Harness

| Symbol | Description |
|--------|-------------|
| `AttemptJudge` | Attempt verdict strategy |
| `Criterion` | Judgment criterion |
| `Verdict` | Judgment result |
| `judge` | LLM judge |

## Signals

| Symbol | Description |
|--------|-------------|
| `SignalGateway` | Signal ingress |
| `RuntimeSignal` | Signal payload |
| `ScheduledPrompt` | Scheduled prompt |

See source `__all__` for the complete list.
