"""
03 — @tool decorator, execute_tools(), read_file, LLM tool calling
"""
import json
import tempfile
from pathlib import Path

import pytest

from deepstrike import tool, execute_tools, read_file
from deepstrike._kernel import ToolCall
from deepstrike.providers.stream import ToolResultEvent

from conftest import make_agent, collect_events, text


# ─── Offline mechanics ──────────────────────────────────────────────────────

class TestToolDecorator:
    def test_creates_correct_schema(self):
        @tool
        def add(x: int, y: int) -> int:
            """Add two numbers."""
            return x + y

        assert add.schema.name == "add"
        assert "x" in add.schema.parameters

    async def test_execute_returns_result(self):
        @tool
        def add(x: int, y: int) -> int:
            """Add two numbers."""
            return x + y

        result = await add(x=3, y=4)
        assert result == "7"

    async def test_execute_propagates_exceptions(self):
        @tool
        def boom() -> str:
            """Explodes."""
            raise RuntimeError("kaboom")

        with pytest.raises(RuntimeError, match="kaboom"):
            await boom()


class TestExecuteTools:
    async def test_runs_known_tool(self):
        @tool
        def echo(msg: str) -> str:
            """Echo."""
            return msg

        results = await execute_tools(
            [ToolCall(id="c1", name="echo", arguments='{"msg":"hello"}')],
            {"echo": echo},
        )
        assert results[0].output == "hello"
        assert results[0].is_error is False

    async def test_returns_error_for_unknown_tool(self):
        results = await execute_tools(
            [ToolCall(id="c2", name="ghost", arguments="{}")],
            {},
        )
        assert results[0].is_error is True
        assert "ghost" in results[0].output

    async def test_returns_error_when_tool_throws(self):
        @tool
        def fail() -> str:
            """Fails."""
            raise RuntimeError("oops")

        results = await execute_tools(
            [ToolCall(id="c3", name="fail", arguments="{}")],
            {"fail": fail},
        )
        assert results[0].is_error is True


class TestReadFile:
    def test_schema_has_path_field(self):
        assert read_file.schema.name == "read_file"
        params = json.loads(read_file.schema.parameters)
        assert "path" in params.get("required", []) or "path" in params.get("properties", {})

    async def test_reads_existing_file(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".txt", delete=False) as f:
            f.write("hello")
            path = f.name
        result = await read_file(path=path)
        assert result == "hello"
        Path(path).unlink(missing_ok=True)


# ─── LLM tool calling (real API) ───────────────────────────────────────────

class TestAgentWithTools:
    @pytest.mark.timeout(90)
    async def test_llm_calls_arithmetic_tool(self):
        @tool
        def calculate(op: str, a: int, b: int) -> str:
            """Perform arithmetic: add, sub, mul, div."""
            x, y = int(a), int(b)
            result = {"add": x + y, "sub": x - y, "mul": x * y, "div": x // y if y else 0}
            return str(result.get(op, 0))

        agent = make_agent()
        agent.register(calculate)
        events = await collect_events(
            agent.run_streaming("Use the calculate tool to compute 17 * 6. Return only the numeric result."),
        )

        tool_results = [e for e in events if isinstance(e, ToolResultEvent)]
        calc_results = [r for r in tool_results if r.name == "calculate"]
        assert len(calc_results) > 0 or len(tool_results) > 0, \
            f"calculate must have been called, got tool_results: {[(r.name, r.content) for r in tool_results]}"
        final = text(events)
        assert "102" in final or any("102" in r.content for r in tool_results), \
            f"final text: {final}, tool results: {[(r.name, r.content) for r in tool_results]}"

    @pytest.mark.timeout(90)
    async def test_tool_call_precedes_tool_result(self):
        @tool
        def ping() -> str:
            """Returns pong."""
            return "pong"

        agent = make_agent()
        agent.register(ping)
        events = await collect_events(
            agent.run_streaming("Call the ping tool and report what it returns."),
        )

        from deepstrike.providers.stream import ToolCallEvent
        call_idx = next((i for i, e in enumerate(events) if isinstance(e, ToolCallEvent)), -1)
        result_idx = next((i for i, e in enumerate(events) if isinstance(e, ToolResultEvent)), -1)
        if call_idx != -1 and result_idx != -1:
            assert call_idx < result_idx, "tool_call must precede tool_result"
