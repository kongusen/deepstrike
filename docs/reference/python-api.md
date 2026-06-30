---
# code_refs: validated by scripts/check-docs-drift.mjs against python __all__ — symbols must exist.
code_refs:
  python: [RuntimeRunner, RuntimeOptions, AnthropicProvider, OpenAIProvider, OpenAIResponsesProvider, LLMProvider, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent, tool, LocalExecutionPlane, WorkflowSpec, WorkflowNodeSpec, Governance, AgentPool, ReactiveSession, RuntimeSignal, run_agent, run_fanout, DeepSeekProvider, QwenProvider, KimiProvider, OllamaProvider, RenderedContext, streaming_tool, read_file, WorkingMemory, DreamStore, MemoryEntry, MemoryPolicy, fanout_synthesize, generate_and_filter, verify_rules, workflow_spec_to_kernel, ResourceQuota, AgentRunSpec, HandoffBus, ContractDrivenHarness, HarnessLoop, SignalGateway, ScheduledPrompt]
---

# Python API 索引

`from deepstrike import ...` 的公开导出，定义于 `python/deepstrike/__init__.py`。

## 入口

| 符号 | 说明 |
|------|------|
| `run_agent` | 单 agent 快捷调用 |
| `run_fanout` | 并行 + 合成 |
| `RuntimeRunner` | 完整 runner |
| `RuntimeOptions` | Runner 配置 |

## Provider

| 符号 | 说明 |
|------|------|
| `AnthropicProvider` | Anthropic API |
| `OpenAIProvider` | OpenAI Chat |
| `OpenAIResponsesProvider` | OpenAI Responses |
| `DeepSeekProvider` | DeepSeek |
| `QwenProvider` | 通义 |
| `KimiProvider` | Kimi |
| `OllamaProvider` | Ollama |
| `LLMProvider` | 基类协议 |
| `RenderedContext` | 渲染后的上下文 |

## 流式事件

| 符号 | 说明 |
|------|------|
| `TextDelta` | 文本增量 |
| `ToolCallEvent` | 工具调用 |
| `ToolResultEvent` | 工具结果 |
| `DoneEvent` | 完成 |
| `ErrorEvent` | 错误 |
| `PermissionRequestEvent` | 治理 ask_user |

## 工具

| 符号 | 说明 |
|------|------|
| `tool` | 装饰器注册工具 |
| `streaming_tool` | 流式工具 |
| `read_file` | 内置读文件 |
| `LocalExecutionPlane` | 本地工具执行 |

## Memory

| 符号 | 说明 |
|------|------|
| `WorkingMemory` | 进程内 scratch |
| `DreamStore` | 协议（实现自定） |
| `InMemoryDreamStore` | 内存实现 |
| `MemoryEntry` | 记忆条目 |
| `MemoryPolicy` | 记忆策略 |

## Workflow

| 符号 | 说明 |
|------|------|
| `WorkflowSpec` | DAG spec |
| `WorkflowNodeSpec` | 节点 spec |
| `fanout_synthesize` | 模板 |
| `generate_and_filter` | 模板 |
| `verify_rules` | 模板 |
| `workflow_spec_to_kernel` | 转 kernel JSON |

## Governance

| 符号 | 说明 |
|------|------|
| `Governance` | 治理 wrapper |
| `GovernancePolicy` | 声明式策略 |
| `ResourceQuota` | 资源配额 |

## Collaboration

| 符号 | 说明 |
|------|------|
| `AgentPool` | 角色 pool |
| `AgentRunSpec` | Sub-agent spec |
| `HandoffBus` | Handoff 总线 |
| `ContractDrivenHarness` | 契约 harness |
| `ReactiveSession` | 多 peer reactive |

## Harness

| 符号 | 说明 |
|------|------|
| `HarnessLoop` | 重试 harness |
| `Criterion` | 评判标准 |
| `Verdict` | 评判结果 |
| `judge` | LLM judge |

## Signals

| 符号 | 说明 |
|------|------|
| `SignalGateway` | 信号入口 |
| `RuntimeSignal` | 信号 payload |
| `ScheduledPrompt` | 定时 prompt |

完整列表见源码 `__all__`。
