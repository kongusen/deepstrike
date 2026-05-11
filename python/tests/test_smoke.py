import pytest
from deepstrike import (
    Agent, AnthropicProvider, OpenAIProvider, OllamaProvider,
    LoopStateMachine, LoopPolicy, RuntimeTask,
    Message, ToolSchema, ToolCall, ToolResult,
    tool, read_file,
    Governance, SignalRouter,
    RetryConfig,
)


def test_kernel_import():
    from deepstrike._kernel import LoopStateMachine, LoopPolicy
    sm = LoopStateMachine(LoopPolicy())
    assert not sm.is_terminal()


def test_tool_decorator():
    @tool
    def add(x: int, y: int) -> int:
        """Add two numbers."""
        return x + y

    assert add.schema.name == "add"
    assert "x" in add.schema.parameters


def test_read_file_tool():
    import tempfile, pathlib
    with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
        f.write("hello")
        path = f.name
    assert read_file.schema.name == "read_file"
    import asyncio
    result = asyncio.run(read_file(path=path))
    assert result == "hello"


def test_governance_block_tool():
    gov = Governance()
    gov.block_tool("dangerous")
    provider = AnthropicProvider(api_key="test")
    agent = Agent(provider, max_tokens=1000, max_turns=3, governance=gov)
    agent.block_tool("dangerous")
    assert "dangerous" in agent._blocked_tools


def test_signal_router():
    router = SignalRouter(max_queue_size=10)
    assert router.depth() == 0
    router.clear_dedup()


def test_agent_block_tool_no_governance():
    provider = AnthropicProvider(api_key="test")
    agent = Agent(provider, max_tokens=1000, max_turns=3)
    agent.block_tool("shell")
    assert "shell" in agent._blocked_tools


def test_provider_instantiation():
    assert OpenAIProvider(api_key="test")._model == "gpt-4o"
    assert OllamaProvider(model="llama3")._model == "llama3"
    assert AnthropicProvider(api_key="test", model="claude-opus-4-7")._model == "claude-opus-4-7"


def test_retry_config_defaults():
    cfg = RetryConfig()
    assert cfg.max_retries == 3
    assert cfg.base_delay == 1.0
    assert cfg.circuit_open_after == 5
