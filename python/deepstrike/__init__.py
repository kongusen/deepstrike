"""Public Agent intent surface. Host machinery lives in deepstrike.advanced."""

from importlib.metadata import PackageNotFoundError as _PackageNotFoundError, version as _version
try:
    __version__ = _version("deepstrike")
except _PackageNotFoundError:
    __version__ = "0+unknown"

from deepstrike._kernel import (ModelMessage, ToolCall, ToolExecutionResult, ToolSchema, SkillMetadata)

from deepstrike.providers import (LLMProvider, AnthropicProvider, OpenAIProvider, OpenAIResponsesProvider)

from deepstrike.tools import (RegisteredTool, tool, streaming_tool, safe_tool, ok, fail, format_tool_error)

from deepstrike.memory import (WorkingMemory, MemoryStore, Memory, MemoryScope)

from deepstrike.knowledge import (KnowledgeSource)

from deepstrike.agent import (Agent, AgentMemory, MemoryReference, ModelRef, PortableRunResult, PortableSession, create_agent)

__all__ = ['Agent', 'create_agent', 'AgentMemory', 'MemoryReference', 'ModelRef', 'PortableRunResult', 'PortableSession', 'WorkingMemory', 'Memory', 'MemoryStore', 'MemoryScope', 'KnowledgeSource', 'LLMProvider', 'AnthropicProvider', 'OpenAIProvider', 'OpenAIResponsesProvider', 'RegisteredTool', 'tool', 'streaming_tool', 'safe_tool', 'ok', 'fail', 'format_tool_error', 'ModelMessage', 'ToolCall', 'ToolExecutionResult', 'ToolSchema', 'SkillMetadata', '__version__']
