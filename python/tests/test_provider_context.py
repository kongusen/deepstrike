from deepstrike._kernel import ContentPartObj, Message, ToolCall
from deepstrike.providers.base import (
    RenderedContext,
    to_anthropic_messages,
    to_openai_message_params,
)


def _context() -> RenderedContext:
    return RenderedContext(
        system_text="system rules",
        turns=[
            Message(role="user", content="What is the weather?"),
            Message(
                role="assistant",
                content="I'll check.",
                tool_calls=[ToolCall("call_1", "get_weather", '{"city":"Shanghai"}')],
            ),
            Message(
                role="tool",
                content="",
                content_parts=[
                    ContentPartObj(
                        "tool_result",
                        call_id="call_1",
                        output="sunny",
                        is_error=False,
                    )
                ],
            ),
        ],
    )


def test_openai_context_replays_tool_calls_and_results_natively():
    assert to_openai_message_params(_context()) == [
        {"role": "system", "content": "system rules"},
        {"role": "user", "content": "What is the weather?"},
        {
            "role": "assistant",
            "content": "I'll check.",
            "tool_calls": [
                {
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": '{"city":"Shanghai"}',
                    },
                }
            ],
        },
        {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
    ]


def test_anthropic_context_replays_tool_calls_and_results_as_blocks():
    assert to_anthropic_messages(_context().turns) == [
        {"role": "user", "content": "What is the weather?"},
        {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll check."},
                {
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "get_weather",
                    "input": {"city": "Shanghai"},
                },
            ],
        },
        {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "sunny",
                    "is_error": False,
                }
            ],
        },
    ]


def test_openai_appends_state_turn_as_latest():
    ctx = RenderedContext(
        system_text="sys",
        turns=[Message(role="user", content="history msg")],
        state_turn=Message(role="user", content="[TASK STATE] goal: g\n\nProceed."),
    )
    msgs = to_openai_message_params(ctx)
    # [system][history][state] — history is the stable cacheable prefix, state last.
    assert msgs[0] == {"role": "system", "content": "sys"}
    assert msgs[1] == {"role": "user", "content": "history msg"}
    assert msgs[2]["role"] == "user" and "[TASK STATE]" in msgs[2]["content"]


def test_anthropic_appends_state_turn_after_cached_history():
    from deepstrike.providers.anthropic import AnthropicProvider

    ctx = RenderedContext(
        system_text="",
        turns=[
            Message(role="user", content="earlier question"),
            Message(role="assistant", content="earlier answer"),
        ],
        state_turn=Message(role="user", content="[TASK STATE] goal: g\n\nProceed."),
    )
    msgs = AnthropicProvider("test-key")._build_messages(ctx.turns, ctx.state_turn)
    # history (2) + state (1) appended last
    assert len(msgs) == 3
    # state turn is the uncached tail: plain string content, no cache_control
    assert msgs[2] == {"role": "user", "content": "[TASK STATE] goal: g\n\nProceed."}
    # history tail carries the rolling read-anchor breakpoint (block-array content)
    def _has_cache(c):
        return isinstance(c, list) and any(isinstance(b, dict) and "cache_control" in b for b in c)
    assert _has_cache(msgs[0]["content"]) or _has_cache(msgs[1]["content"])


def test_anthropic_pins_deep_breakpoint_at_frozen_boundary():
    """P1-E: with frozen_prefix_len set, the deep breakpoint pins at the frozen
    boundary and the other rolls at the tail (mirrors the Node golden)."""
    from deepstrike.providers.anthropic import AnthropicProvider

    ctx = RenderedContext(
        system_text="rules",
        system_stable="rules",
        turns=[
            Message(role="user", content="t0 frozen"),
            Message(role="assistant", content="t1 frozen"),
            Message(role="user", content="t2 hot"),
            Message(role="assistant", content="t3 hot"),
            Message(role="user", content="t4 hot tail"),
        ],
        frozen_prefix_len=2,
    )

    def _has_cache(c):
        return isinstance(c, list) and any(isinstance(b, dict) and "cache_control" in b for b in c)

    msgs = AnthropicProvider("test-key")._build_messages(ctx.turns, ctx.state_turn, ctx.frozen_prefix_len)
    # Deep anchor at the last frozen turn (index 1) + rolling tail (index 4).
    assert _has_cache(msgs[1]["content"])
    assert _has_cache(msgs[4]["content"])
    assert not _has_cache(msgs[0]["content"])
    assert not _has_cache(msgs[2]["content"])
    assert not _has_cache(msgs[3]["content"])


def test_anthropic_falls_back_to_rolling_pair_without_frozen_len():
    """P1-E dual-path: no frozen_prefix_len ⇒ rolling pair (tail + nearest preceding user)."""
    from deepstrike.providers.anthropic import AnthropicProvider

    ctx = RenderedContext(
        system_text="rules",
        system_stable="rules",
        turns=[
            Message(role="user", content="a"),
            Message(role="assistant", content="b"),
            Message(role="user", content="c"),
        ],
    )

    def _has_cache(c):
        return isinstance(c, list) and any(isinstance(b, dict) and "cache_control" in b for b in c)

    msgs = AnthropicProvider("test-key")._build_messages(ctx.turns, ctx.state_turn, ctx.frozen_prefix_len)
    assert _has_cache(msgs[2]["content"])  # rolling tail
    assert _has_cache(msgs[0]["content"])  # nearest preceding user
    assert not _has_cache(msgs[1]["content"])
