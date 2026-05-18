import pytest
from deepstrike import (
    Agent, AnthropicProvider, OpenAIProvider, OllamaProvider,
    Message, ToolSchema, ToolCall, ToolResult,
    tool, read_file,
    Governance,
    RetryConfig,
)
from deepstrike.kernel import LoopStateMachine, LoopPolicy, RuntimeTask, SignalRouter
from deepstrike.providers.stream import TextDelta


def test_kernel_import():
    from deepstrike.kernel import LoopStateMachine, LoopPolicy
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
    result = agent.block_tool("dangerous")
    assert result is agent
    verdict = agent._governance.evaluate("dangerous", "{}")
    assert verdict.kind == "deny"


def test_governance_full_pipeline_methods():
    gov = Governance("deny")
    gov.add_permission_rule("safe_*", "allow")
    gov.set_rate_limit("safe_tool", max_calls=1, window_ms=1_000)
    gov.require_param("safe_tool", "path")
    gov.set_time(1_000)

    verdict = gov.evaluate("safe_tool", '{"path": "README.md"}')
    assert verdict.kind == "allow"

    denied = gov.evaluate("unsafe_tool", "{}")
    assert denied.kind == "deny"


def test_signal_router():
    router = SignalRouter(max_queue_size=10)
    assert router.depth() == 0
    router.clear_dedup()


@pytest.mark.xfail(reason="requires current Governance kernel binary; stale local .so predates Governance API")
def test_agent_block_tool_no_governance():
    provider = AnthropicProvider(api_key="test")
    agent = Agent(provider, max_tokens=1000, max_turns=3)
    result = agent.block_tool("shell")
    assert result is agent  # chainable


def test_provider_instantiation():
    assert OpenAIProvider(api_key="test")._model == "gpt-4o"
    assert OllamaProvider(model="llama3")._model == "llama3"
    assert AnthropicProvider(api_key="test", model="claude-opus-4-7")._model == "claude-opus-4-7"


def test_retry_config_defaults():
    cfg = RetryConfig()
    assert cfg.max_retries == 3
    assert cfg.base_delay == 1.0
    assert cfg.circuit_open_after == 5


@pytest.mark.asyncio
async def test_agent_run_returns_model_text():
    class FakeProvider:
        async def complete(self, messages, tools):
            raise NotImplementedError

        async def stream(self, messages, tools, extensions=None):
            yield TextDelta(delta="pong")

    agent = Agent(FakeProvider(), max_tokens=1000, max_turns=3)
    assert await agent.run("ping") == "pong"
