"""spc_012 Python track: structured tool result.

- P-02: regression lock for the silent image-drop bug that lived at
  `runtime/mcp_proxy_plane.py` — `execute()` used to keep only `type == "text"` blocks,
  so an MCP tool returning a screenshot lost the image with no trace (INV-012-01
  violation, same bug as Node mcp-proxy-plane.ts).
- P-03: the Anthropic provider's tool_result serialization reads structured
  `content_parts` when present (native text/image blocks), falling back to the `output`
  text projection otherwise; plus the runner side-channel end-to-end.
"""
from __future__ import annotations

import asyncio

from deepstrike._kernel import Message
from deepstrike.providers.base import RenderedContext, to_anthropic_messages
from deepstrike.providers.stream import ToolResultEvent
from deepstrike.runtime.mcp_proxy_plane import mcp_result_to_tool_output
from deepstrike.runtime.runner import RuntimeRunner
from deepstrike.types.content import RenderedMessage, StructuredToolResultPart


class TestMcpResultToToolOutput:
    def test_image_block_preserved_in_content_parts(self):
        output, is_error, content_parts = mcp_result_to_tool_output({
            "content": [
                {"type": "text", "text": "here is the screenshot"},
                {"type": "image", "data": "aGVsbG8=", "mimeType": "image/png"},
            ],
            "isError": False,
        })
        assert output == "here is the screenshot\n[image]"
        assert is_error is False
        assert content_parts == [
            {"type": "text", "text": "here is the screenshot"},
            {"type": "image", "source": {"kind": "base64", "data": "aGVsbG8="}, "media_type": "image/png"},
        ]

    def test_unknown_block_type_serialized_as_text_not_dropped(self):
        weird = {"type": "resource", "uri": "file:///tmp/x"}
        _, _, content_parts = mcp_result_to_tool_output({"content": [weird]})
        assert content_parts == [{"type": "text", "text": '{"type": "resource", "uri": "file:///tmp/x"}'}]

    def test_pure_text_response_gets_no_content_parts(self):
        output, is_error, content_parts = mcp_result_to_tool_output({
            "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
            "isError": True,
        })
        assert output == "a\nb"
        assert is_error is True
        assert content_parts is None


def _tool_message(content_parts=None) -> RenderedMessage:
    part = StructuredToolResultPart(
        call_id="call_1",
        output="weather: sunny\n[image]",
        is_error=False,
        content_parts=content_parts,
    )
    return RenderedMessage(role="tool", content="weather: sunny\n[image]", tool_calls=[], content_parts=[part])


class TestAnthropicStructuredToolResult:
    def test_content_parts_serialized_as_structured_blocks(self):
        msgs = to_anthropic_messages([_tool_message([
            {"type": "text", "text": "weather: sunny"},
            {"type": "image", "source": {"kind": "base64", "data": "aGVsbG8="}, "media_type": "image/png"},
        ])])
        assert msgs == [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "is_error": False,
                "content": [
                    {"type": "text", "text": "weather: sunny"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}},
                ],
            }],
        }]

    def test_output_fallback_when_no_content_parts(self):
        msgs = to_anthropic_messages([_tool_message(None)])
        assert msgs == [{
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "call_1", "content": "weather: sunny\n[image]", "is_error": False}],
        }]


class _MultimodalToolPlane:
    """Execution plane whose tool result carries structured content (MCP-screenshot style)."""

    def __init__(self):
        from deepstrike._kernel import ToolSchema
        self._schema = ToolSchema(
            name="screenshot",
            description="Returns a screenshot image",
            parameters='{"type": "object", "properties": {}}',
        )

    def register(self, *tools):
        return self

    def unregister(self, name):
        return self

    def schemas(self):
        return [self._schema]

    async def execute_all(self, calls, ctx):
        for call in calls:
            yield ToolResultEvent(
                call_id=call.id,
                name=call.name,
                content="screenshot taken\n[image]",
                is_error=False,
                content_parts=[
                    {"type": "text", "text": "screenshot taken"},
                    {"type": "image", "source": {"kind": "base64", "data": "aGVsbG8="}, "media_type": "image/png"},
                ],
            )


class _CapturingProvider:
    def __init__(self):
        self.contexts: list[RenderedContext] = []
        self._calls = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done", tool_calls=[])

    async def stream(self, context, tools, extensions=None, state=None, signal=None):
        from deepstrike.providers.stream import TextDelta, ToolCallEvent
        self.contexts.append(context)
        self._calls += 1
        if self._calls == 1:
            yield ToolCallEvent(id="call_1", name="screenshot", arguments={})
            return
        yield TextDelta(delta="done")


def _make_runner(provider, plane):
    from deepstrike.runtime.runner import RuntimeOptions
    from deepstrike.runtime.session_log import InMemorySessionLog
    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=4000,
        max_turns=4,
        baseline_tool_ids=[schema.name for schema in plane.schemas()],
    ))


class TestStructuredToolResultEndToEnd:
    def test_image_block_reaches_next_provider_request_structured(self):
        provider = _CapturingProvider()
        runner = _make_runner(provider, _MultimodalToolPlane())

        async def drain():
            async for _ in runner.run(session_id="spc012-e2e", goal="Take a screenshot."):
                pass

        asyncio.run(drain())

        assert len(provider.contexts) >= 2
        follow_up = provider.contexts[1]
        turns = [*follow_up.turns, follow_up.state_turn] if follow_up.state_turn is not None else follow_up.turns
        tool_msg = next((m for m in turns if m.role == "tool"), None)
        assert tool_msg is not None
        part = next(
            (p for p in (getattr(tool_msg, "content_parts", None) or [])
             if getattr(p, "type", None) == "tool_result" and getattr(p, "call_id", None) == "call_1"),
            None,
        )
        assert part is not None
        # Canonical durable blocks survive the kernel round trip.
        assert part.content_parts == [
            {"type": "text", "text": "screenshot taken"},
            {"type": "image", "source": {"kind": "base64", "data": "aGVsbG8="}, "media_type": "image/png"},
        ]

        wire = to_anthropic_messages([tool_msg])
        assert wire == [{
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "call_1",
                "is_error": False,
                "content": [
                    {"type": "text", "text": "screenshot taken"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}},
                ],
            }],
        }]


class _TwoSessionProvider(_CapturingProvider):
    async def stream(self, context, tools, extensions=None, state=None, signal=None):
        from deepstrike.providers.stream import TextDelta, ToolCallEvent
        self.contexts.append(context)
        self._calls += 1
        if self._calls % 2 == 1:
            yield ToolCallEvent(id="call_1", name="screenshot", arguments={})
            return
        yield TextDelta(delta="done")


class _FirstStructuredThenTextPlane(_MultimodalToolPlane):
    def __init__(self):
        super().__init__()
        self._executions = 0

    async def execute_all(self, calls, ctx):
        for call in calls:
            self._executions += 1
            yield ToolResultEvent(
                call_id=call.id,
                name=call.name,
                content="first\n[image]" if self._executions == 1 else "second text only",
                is_error=False,
                content_parts=(
                    [
                        {"type": "text", "text": "first"},
                        {"type": "image", "source": {"kind": "base64", "data": "Zmlyc3Q="}, "media_type": "image/png"},
                    ]
                    if self._executions == 1 else None
                ),
            )


def test_same_runner_two_sessions_do_not_share_reused_call_id_blocks():
    provider = _TwoSessionProvider()
    runner = _make_runner(provider, _FirstStructuredThenTextPlane())

    async def drain():
        async for _ in runner.run(session_id="first-session", goal="first"):
            pass
        async for _ in runner.run(session_id="second-session", goal="second"):
            pass

    asyncio.run(drain())

    second_follow_up = provider.contexts[3]
    turns = [*second_follow_up.turns, second_follow_up.state_turn] if second_follow_up.state_turn is not None else second_follow_up.turns
    tool_msg = next(message for message in turns if message.role == "tool")
    part = next(
        item for item in (getattr(tool_msg, "content_parts", None) or [])
        if getattr(item, "type", None) == "tool_result" and getattr(item, "call_id", None) == "call_1"
    )
    assert getattr(part, "content_parts", None) is None
