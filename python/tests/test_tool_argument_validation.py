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


def _wrap(prop: dict) -> str:
    return json.dumps({"type": "object", "properties": {"v": prop}, "required": ["v"]})


def test_min_max_length_enforce_string_bounds():
    schema = _wrap({"type": "string", "minLength": 2, "maxLength": 4})
    assert validate_tool_arguments(schema, {"v": "ok"})["error"] is None
    assert validate_tool_arguments(schema, {"v": "x"})["error"] == "$.v must be at least 2 characters"
    assert validate_tool_arguments(schema, {"v": "toolong"})["error"] == "$.v must be at most 4 characters"


def test_pattern_enforces_unanchored_regex_and_bad_author_regex_never_fails():
    schema = _wrap({"type": "string", "pattern": "^[a-z]+$"})
    assert validate_tool_arguments(schema, {"v": "abc"})["error"] is None
    assert validate_tool_arguments(schema, {"v": "ABC"})["error"] == "$.v must match pattern ^[a-z]+$"
    bad_regex = _wrap({"type": "string", "pattern": "(["})
    assert validate_tool_arguments(bad_regex, {"v": "anything"})["error"] is None


def test_numeric_bounds_including_exclusive():
    schema = _wrap({"type": "number", "minimum": 0, "exclusiveMaximum": 10})
    assert validate_tool_arguments(schema, {"v": 0})["error"] is None
    assert validate_tool_arguments(schema, {"v": -1})["error"] == "$.v must be >= 0"
    assert validate_tool_arguments(schema, {"v": 10})["error"] == "$.v must be < 10"


def test_bounds_apply_after_string_to_number_auto_cast():
    schema = _wrap({"type": "integer", "minimum": 1})
    assert validate_tool_arguments(schema, {"v": "0"})["error"] == "$.v must be >= 1"


def test_min_max_items_enforce_array_cardinality():
    schema = _wrap({"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 2})
    assert validate_tool_arguments(schema, {"v": ["a"]})["error"] is None
    assert validate_tool_arguments(schema, {"v": []})["error"] == "$.v must have at least 1 items"
    assert validate_tool_arguments(schema, {"v": ["a", "b", "c"]})["error"] == "$.v must have at most 2 items"


def test_not_rejects_disallowed_shape():
    schema = _wrap({"type": "string", "not": {"enum": ["forbidden"]}})
    assert validate_tool_arguments(schema, {"v": "fine"})["error"] is None
    assert validate_tool_arguments(schema, {"v": "forbidden"})["error"] == "$.v must not match the disallowed shape"


def test_min_length_inside_union_branch_participates_in_discrimination():
    schema = json.dumps({
        "type": "object",
        "properties": {"m": {"type": "string"}},
        "oneOf": [
            {"type": "object", "properties": {"m": {"type": "string", "minLength": 1}}, "required": ["m"]},
        ],
    })
    assert validate_tool_arguments(schema, {"m": "hello"})["error"] is None
    assert validate_tool_arguments(schema, {"m": ""})["error"] == "$ does not match any allowed shape"


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
