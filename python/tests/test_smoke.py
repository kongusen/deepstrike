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
from deepstrike.governance import GovernancePolicy, GovernancePolicyRule
from deepstrike.providers.stream import (
    PermissionResolvedEvent,
    TextDelta,
    ToolCallEvent,
    ToolDeniedEvent,
    ToolResultEvent,
)


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


def test_validate_tool_arguments_additional_properties_true_keeps_keys():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({
        "type": "object",
        "properties": {
            "bag": {"type": "object", "additionalProperties": True, "properties": {"kind": {"type": "string"}}}
        },
    })
    args = {"bag": {"kind": "a", "anyKey": {"nested": 1}, "x": [1, 2]}}
    validation = validate_tool_arguments(schema, args)
    assert validation["error"] is None
    assert args["bag"] == {"kind": "a", "anyKey": {"nested": 1}, "x": [1, 2]}  # untouched


def test_validate_tool_arguments_additional_properties_undefined_still_strips():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({"type": "object", "properties": {"a": {"type": "string"}}})
    args = {"a": "x", "extra": 1}
    validation = validate_tool_arguments(schema, args)
    assert validation["error"] is None
    assert args == {"a": "x"}  # back-compat: extra trimmed


def test_validate_tool_arguments_additional_properties_subschema():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({"type": "object", "properties": {}, "additionalProperties": {"type": "number"}})
    args = {"a": "10", "b": 2}  # "10" auto-cast to 10
    validation = validate_tool_arguments(schema, args)
    assert validation["error"] is None
    assert args == {"a": 10, "b": 2}

    bad = {"a": {"not": "a number"}}
    assert validate_tool_arguments(schema, bad)["error"] is not None


def test_validate_tool_arguments_coerce_item_array():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({
        "type": "object",
        "properties": {
            "ops": {"type": "array", "items": {
                "type": "object", "properties": {"op": {"type": "string"}}, "required": ["op"]}}
        },
        "required": ["ops"],
    })

    # {"item": [...]} unwraps
    a = {"ops": {"item": [{"op": "add"}, {"op": "remove"}]}}
    r = validate_tool_arguments(schema, a)
    assert r["error"] is None and r["repaired"] is True
    assert a["ops"] == [{"op": "add"}, {"op": "remove"}]

    # {"items": {obj}} wraps a single object
    b = {"ops": {"items": {"op": "add"}}}
    assert validate_tool_arguments(schema, b)["error"] is None
    assert b["ops"] == [{"op": "add"}]

    # lone object wraps
    c = {"ops": {"op": "add"}}
    assert validate_tool_arguments(schema, c)["error"] is None
    assert c["ops"] == [{"op": "add"}]

    # precise per-element error restored after coercion
    d = {"ops": {"item": {"path": "/x"}}}
    assert validate_tool_arguments(schema, d)["error"] == "$.ops[0].op is required"

    # well-formed array untouched (no repair)
    e = {"ops": [{"op": "add"}]}
    re = validate_tool_arguments(schema, e)
    assert re["error"] is None and re["repaired"] is False
    assert e["ops"] == [{"op": "add"}]


def test_validate_tool_arguments_oneof_polymorphic():
    from deepstrike.tools import validate_tool_arguments
    import json

    schema = json.dumps({
        "type": "object",
        "properties": {
            "text": {"oneOf": [
                {"type": "string"},
                {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
            ]}
        },
        "required": ["text"],
    })

    scalar = {"text": "hello"}
    assert validate_tool_arguments(schema, scalar)["error"] is None
    assert scalar["text"] == "hello"

    binding = {"text": {"path": "/k"}}
    assert validate_tool_arguments(schema, binding)["error"] is None
    assert binding["text"] == {"path": "/k"}

    bad = {"text": 123}
    assert validate_tool_arguments(schema, bad)["error"] is not None


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
    assert GLMProvider(api_key="test")._model == "glm-5.2"
    assert GLMProvider(api_key="test", model="glm/glm-4-plus").runtime_policy().max_turns == 35
    from deepstrike.providers import GeminiProvider, KimiProvider, MiniMaxAnthropicProvider, QwenProvider
    assert AnthropicProvider(api_key="test", model="claude-opus-4-1").runtime_policy().max_turns == 50
    assert OpenAIProvider(api_key="test", model="gpt-5.5").runtime_policy().max_turns == 60
    assert MiniMaxAnthropicProvider(api_key="test", model="MiniMax-M2.7-highspeed").runtime_policy().max_turns == 35
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
    assert await collect_text(runner.run(goal="ping")) == "pong"


class AskUserProvider:
    def __init__(self) -> None:
        self.calls = 0

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        if self.calls == 1:
            yield ToolCallEvent(id="call_approval", name="needs_approval", arguments={})
            return
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_ask_user_gated_tool_runs_after_host_approval():
    from deepstrike.governance import GovernancePolicy, GovernancePolicyRule
    executed = False

    @tool
    def needs_approval() -> str:
        nonlocal executed
        executed = True
        return "approved-result"

    plane = LocalExecutionPlane().register(needs_approval)
    log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=AskUserProvider(),
        session_log=log,
        execution_plane=plane,
        max_tokens=1000,
        max_turns=2,
        governance_policy=GovernancePolicy(
            rules=[GovernancePolicyRule(pattern="needs_approval", action="ask_user")],
        ),
        on_permission_request=lambda request: {
            "approved": request.tool_name == "needs_approval",
            "responder": "test-host",
        },
    ))

    events = [event async for event in runner.run(session_id="ask-user-approved", goal="run approved tool")]

    assert executed is True
    assert any(isinstance(event, PermissionResolvedEvent) and event.approved and event.responder == "test-host" for event in events)
    assert any(isinstance(event, ToolResultEvent) and event.call_id == "call_approval" and event.content == "approved-result" and not event.is_error for event in events)
    log_events = [entry.event for entry in await log.read("ask-user-approved")]
    assert any(event.get("kind") == "permission_resolved" and event.get("approved") is True for event in log_events)


@pytest.mark.asyncio
async def test_ask_user_gated_tool_is_denied_after_host_rejection():
    from deepstrike.governance import GovernancePolicy, GovernancePolicyRule

    executed = False

    @tool
    def needs_approval() -> str:
        nonlocal executed
        executed = True
        return "should-not-run"

    plane = LocalExecutionPlane().register(needs_approval)
    runner = RuntimeRunner(RuntimeOptions(
        provider=AskUserProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=1000,
        max_turns=2,
        governance_policy=GovernancePolicy(
            rules=[GovernancePolicyRule(pattern="needs_approval", action="ask_user")],
        ),
        on_permission_request=lambda request: {
            "approved": False,
            "responder": "test-host",
            "reason": "user declined",
        },
    ))

    events = [event async for event in runner.run(session_id="ask-user-denied", goal="run rejected tool")]

    assert executed is False
    assert any(isinstance(event, PermissionResolvedEvent) and not event.approved and event.reason == "user declined" for event in events)
    assert any(isinstance(event, ToolDeniedEvent) and event.tool_name == "needs_approval" and event.reason == "user declined" for event in events)


@pytest.mark.asyncio
async def test_session_log_primitive_filter():
    from deepstrike.runtime.session_log import InMemorySessionLog
    log = InMemorySessionLog()
    await log.append("s1", {"kind": "run_started", "run_id": "r1", "goal": "hi"})
    await log.append("s1", {"kind": "page_out", "turn": 0, "category": "mm", "primitive": "mm", "summary": "po"})
    await log.append("s1", {"kind": "suspended", "turn": 1, "category": "sched", "primitive": "sched", "reason": "sus"})
    await log.append("s1", {"kind": "tool_gated", "turn": 2, "category": "syscall", "primitive": "syscall", "call_id": "c1", "tool": "t1", "reason": "gated"})

    mm_events = await log.read("s1", 0, "mm")
    assert len(mm_events) == 1
    assert mm_events[0].event["kind"] == "page_out"

    sched_events = await log.read("s1", 1, "sched")
    assert len(sched_events) == 1
    assert sched_events[0].event["kind"] == "suspended"

    syscall_events = await log.read("s1", 0, "syscall")
    assert len(syscall_events) == 1
    assert syscall_events[0].event["kind"] == "tool_gated"
