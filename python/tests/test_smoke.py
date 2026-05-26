import pytest
from deepstrike import (
    AnthropicProvider, InMemorySessionLog, LocalExecutionPlane, OpenAIProvider, OllamaProvider,
    RuntimeOptions, RuntimeRunner, collect_text,
    Message, ToolSchema, ToolCall, ToolResult,
    tool, read_file,
    Governance,
    RetryConfig,
)
from deepstrike.kernel import KernelRuntime, LoopPolicy, RuntimeTask, SignalRouter
from deepstrike.providers.stream import TextDelta


def test_kernel_import():
    from deepstrike.kernel import KernelRuntime, LoopPolicy
    runtime = KernelRuntime(LoopPolicy())
    assert not runtime.is_terminal()


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


def test_validate_tool_arguments_repair():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({
        "type": "object",
        "properties": {
            "count": { "type": "integer" },
            "enabled": { "type": "boolean" },
            "ratio": { "type": "number", "default": 0.5 }
        },
        "required": ["count"]
    })

    # 1. 成功自愈
    args = {
        "count": "10",
        "enabled": "true",
        "extra_field": "remove_me"
    }
    validation = validate_tool_arguments(schema, args)
    assert validation["error"] is None
    assert validation["repaired"] is True
    assert args["count"] == 10
    assert args["enabled"] is True
    assert args["ratio"] == 0.5
    assert "extra_field" not in args

    # 2. 无法自愈 (缺失 required)
    args_invalid = {
        "enabled": False
    }
    validation_invalid = validate_tool_arguments(schema, args_invalid)
    assert validation_invalid["error"] is not None


@pytest.mark.asyncio
async def test_execution_plane_repairs_arguments():
    import json
    from deepstrike import LocalExecutionPlane, tool
    from deepstrike.runtime.execution_plane import RunContext
    from deepstrike.providers.stream import ToolArgumentRepairedEvent, ToolResultEvent

    plane = LocalExecutionPlane()
    @tool
    def test_repair(count: int, enabled: bool, ratio: float = 0.5) -> str:
        """Test repair"""
        return json.dumps({"count": count, "enabled": enabled, "ratio": ratio})

    test_repair.schema.parameters = json.dumps({
        "type": "object",
        "properties": {
            "count": { "type": "integer" },
            "enabled": { "type": "boolean" },
            "ratio": { "type": "number", "default": 0.5 }
        },
        "required": ["count"]
    })

    plane.register(test_repair)

    events = []
    async for evt in plane.execute_all(
        [ToolCall(id="c1", name="test_repair", arguments=json.dumps({"count": "10", "enabled": "true", "extra_field": "remove_me"}))],
        RunContext()
    ):
        events.append(evt)

    # 验证投递了自愈事件
    repaired_events = [e for e in events if isinstance(e, ToolArgumentRepairedEvent)]
    assert len(repaired_events) == 1
    assert repaired_events[0].call_id == "c1"
    assert repaired_events[0].name == "test_repair"
    assert json.loads(repaired_events[0].repaired_arguments) == {"count": 10, "enabled": True, "ratio": 0.5}

    # 验证最终执行正确
    result_events = [e for e in events if isinstance(e, ToolResultEvent)]
    assert len(result_events) == 1
    assert result_events[0].call_id == "c1"
    assert json.loads(result_events[0].content) == {"count": 10, "enabled": True, "ratio": 0.5}
    assert not result_events[0].is_error


def test_governance_block_tool():
    gov = Governance()
    gov.block_tool("dangerous")
    verdict = gov.evaluate("dangerous", "{}")
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


def test_provider_instantiation():
    assert OpenAIProvider(api_key="test")._model == "gpt-4o"
    assert OllamaProvider(model="llama3")._model == "llama3"
    assert AnthropicProvider(api_key="test", model="claude-opus-4-7")._model == "claude-opus-4-7"
    from deepstrike.providers import GLMProvider
    assert GLMProvider(api_key="test")._model == "glm-5.1"
    assert GLMProvider(api_key="test", model="glm/glm-4-plus").runtime_policy().max_turns == 35
    from deepstrike.providers import GeminiProvider, KimiProvider, MiniMaxProvider, QwenProvider
    assert AnthropicProvider(api_key="test", model="claude-opus-4-1").runtime_policy().max_turns == 50
    assert OpenAIProvider(api_key="test", model="gpt-5.5").runtime_policy().max_turns == 60
    assert MiniMaxProvider(api_key="test", model="MiniMax-M2.7-highspeed").runtime_policy().max_turns == 35
    assert KimiProvider(api_key="test", model="kimi-k2-thinking").runtime_policy().max_turns == 50
    assert QwenProvider(api_key="test", model="qwen3.7-max-preview").runtime_policy().max_turns == 45
    assert QwenProvider(api_key="test", model="qwen3.5-plus").runtime_policy().max_turns == 35
    assert GeminiProvider(api_key="test", model="gemini-3.5-flash").runtime_policy().max_turns == 30
    assert OpenAIProvider(api_key="test", model="gpt-next-custom", base_url="https://gateway.example.com/v1")._base_url == "https://gateway.example.com/v1"
    assert QwenProvider(api_key="test", model="qwen-next-custom", base_url="https://dashscope-gateway.example.com/v1")._base_url == "https://dashscope-gateway.example.com/v1"
    assert GeminiProvider(api_key="test", model="gemini-next-custom", base_url="https://gemini-gateway.example.com")._base_url == "https://gemini-gateway.example.com"


def test_retry_config_defaults():
    cfg = RetryConfig()
    assert cfg.max_retries == 3
    assert cfg.base_delay == 1.0
    assert cfg.circuit_open_after == 5


@pytest.mark.asyncio
async def test_agent_run_returns_model_text():
    class FakeProvider:
        async def complete(self, context, tools, extensions=None):
            raise NotImplementedError

        async def stream(self, context, tools, extensions=None, state=None):
            yield TextDelta(delta="pong")

    runner = RuntimeRunner(RuntimeOptions(
        provider=FakeProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=1000,
        max_turns=3,
    ))
    assert await collect_text(runner.run_streaming("ping")) == "pong"
