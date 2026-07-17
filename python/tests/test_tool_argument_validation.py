"""Union-root argument validation (mirrors node/tests/tool-argument-validation.test.ts).

A oneOf/anyOf at the schema ROOT accepts a repaired probe deep-copy — the caller's dict never
sees those repairs, so execute paths must use the validator's returned ``args``. ``const`` is
the discriminator convention for union variants: without checking it, the wrong branch could
match on required+type alone and strip the right branch's keys.
"""
import json

import pytest

from deepstrike._kernel import ToolCall, ToolSchema
from deepstrike.tools.execution import execute_tools
from deepstrike.tools.registry import RegisteredTool, validate_tool_arguments


INLINE_RESULT_SHAPED = json.dumps({
    "type": "object",
    "properties": {
        "kind": {"enum": ["edit", "discuss"]},
        "delivery": {"enum": ["content", "tool"]},
        "documentContent": {},
        "summary": {"type": "string"},
        "assistantMessage": {"type": "string"},
    },
    "required": ["kind"],
    "oneOf": [
        {
            "type": "object",
            "properties": {
                "kind": {"const": "edit"},
                "delivery": {"const": "content"},
                "documentContent": {"type": "string"},
                "summary": {"type": "string"},
            },
            "required": ["kind", "delivery", "documentContent"],
        },
        {
            "type": "object",
            "properties": {
                "kind": {"const": "edit"},
                "delivery": {"const": "tool"},
                "documentContent": {"type": "null"},
                "summary": {"type": "string"},
            },
            "required": ["kind", "delivery", "documentContent"],
        },
        {
            "type": "object",
            "properties": {
                "kind": {"const": "discuss"},
                "assistantMessage": {"type": "string"},
            },
            "required": ["kind", "assistantMessage"],
        },
    ],
})


def test_const_discriminates_branches():
    # Without const checking, branch 1 (kind=edit) matched this discuss-shaped call on
    # required+type alone and stripped assistantMessage — silent data mangling.
    args = {"kind": "discuss", "delivery": "content", "documentContent": "stray", "assistantMessage": "the actual answer"}
    r = validate_tool_arguments(INLINE_RESULT_SHAPED, args)
    assert r["error"] is None
    assert r["args"] == {"kind": "discuss", "assistantMessage": "the actual answer"}


def test_type_null_is_enforced():
    ok = validate_tool_arguments(INLINE_RESULT_SHAPED, {"kind": "edit", "delivery": "tool", "documentContent": None})
    assert ok["error"] is None
    bad = validate_tool_arguments(INLINE_RESULT_SHAPED, {"kind": "edit", "delivery": "tool", "documentContent": "not null"})
    assert bad["error"] is not None


def test_union_root_repairs_live_on_returned_args_not_the_original():
    original = {"kind": "discuss", "assistantMessage": "hi", "hallucinated": True}
    r = validate_tool_arguments(INLINE_RESULT_SHAPED, original)
    assert r["error"] is None
    assert r["repaired"] is True
    assert r["args"] == {"kind": "discuss", "assistantMessage": "hi"}
    # The repair (key strip) lives on the returned deep-copy, NOT the original reference.
    assert original["hallucinated"] is True


@pytest.mark.asyncio
async def test_execute_tools_hands_handler_the_repaired_union_root_args():
    seen: list[dict] = []

    def finish(**kwargs):
        seen.append(kwargs)
        return "ok"

    registered = RegisteredTool(finish, ToolSchema(
        name="finish", description="terminal", parameters=INLINE_RESULT_SHAPED,
    ))
    results = await execute_tools(
        [ToolCall(id="c1", name="finish", arguments=json.dumps(
            {"kind": "discuss", "assistantMessage": "done", "hallucinated": 1},
        ))],
        {"finish": registered},
    )
    assert results[0].is_error is not True
    assert seen[0] == {"kind": "discuss", "assistantMessage": "done"}
