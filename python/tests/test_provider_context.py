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
