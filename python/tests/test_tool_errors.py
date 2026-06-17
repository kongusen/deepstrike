import json
import warnings

import pytest

from deepstrike import (
    LocalExecutionPlane,
    ToolError,
    execute_tools,
    fail,
    format_tool_error,
    ok,
    safe_tool,
    streaming_tool,
    tool,
)
from deepstrike._kernel import ToolCall
from deepstrike.providers.stream import ToolAuditFailedEvent, ToolResultEvent
from deepstrike.runtime.execution_plane import RunContext


# ── format_tool_error ──────────────────────────────────────────────────────────

def test_format_tool_error_returns_plain_message_for_bare_exception():
    assert format_tool_error(ValueError("bad input")) == "bad input"


def test_format_tool_error_returns_json_for_coded_exception():
    exc = RuntimeError("no such section")
    exc.code = "not_found"
    exc.hint = "call document_outline first"
    out = format_tool_error(exc)
    parsed = json.loads(out)
    assert parsed["message"] == "no such section"
    assert parsed["code"] == "not_found"
    assert parsed["hint"] == "call document_outline first"


def test_format_tool_error_handles_plain_dict_no_object_repr():
    out = format_tool_error({"kind": "weird", "n": 1})
    assert json.loads(out) == {"kind": "weird", "n": 1}


def test_format_tool_error_passes_through_string():
    assert format_tool_error("boom") == "boom"


def test_format_tool_error_handles_none():
    assert format_tool_error(None) == "None"


def test_format_tool_error_propagates_cause_message():
    inner = ValueError("disk full")
    try:
        raise RuntimeError("write failed") from inner
    except RuntimeError as outer:
        parsed = json.loads(format_tool_error(outer))
        assert parsed["message"] == "write failed"
        assert parsed["cause"] == "disk full"


# ── safe_tool ──────────────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_safe_tool_wraps_plain_return_in_ok_envelope():
    @safe_tool
    def echo(x: str) -> str:
        return x

    [result] = await execute_tools(
        [ToolCall(id="1", name="echo", arguments=json.dumps({"x": "hi"}))],
        {"echo": echo},
    )
    assert result.is_error is False
    assert json.loads(result.output) == {"success": True, "data": "hi"}


@pytest.mark.asyncio
async def test_safe_tool_passes_through_explicit_envelopes():
    @safe_tool
    def lookup(id: str) -> dict:
        if id == "good":
            return ok({"found": True})
        return fail("not_found", f"no row {id}", "list rows via /index")

    [ok_r] = await execute_tools(
        [ToolCall(id="1", name="lookup", arguments=json.dumps({"id": "good"}))],
        {"lookup": lookup},
    )
    assert json.loads(ok_r.output) == {"success": True, "data": {"found": True}}

    [bad_r] = await execute_tools(
        [ToolCall(id="2", name="lookup", arguments=json.dumps({"id": "missing"}))],
        {"lookup": lookup},
    )
    assert json.loads(bad_r.output) == {
        "success": False, "code": "not_found",
        "error": "no row missing", "hint": "list rows via /index",
    }


@pytest.mark.asyncio
async def test_safe_tool_converts_tool_error_throw_into_fail_envelope():
    @safe_tool
    def section_read(heading: str) -> str:
        raise ToolError(
            f'no section "{heading}"',
            code="not_found",
            hint="call document_outline to list valid headings",
        )

    [result] = await execute_tools(
        [ToolCall(id="1", name="section_read", arguments=json.dumps({"heading": "X"}))],
        {"section_read": section_read},
    )
    assert result.is_error is False  # envelope encodes failure; no isError flip
    assert json.loads(result.output) == {
        "success": False,
        "code": "not_found",
        "error": 'no section "X"',
        "hint": "call document_outline to list valid headings",
    }


@pytest.mark.asyncio
async def test_safe_tool_uses_internal_code_for_plain_exception():
    @safe_tool
    def crash() -> str:
        raise RuntimeError("kaboom")

    [r] = await execute_tools(
        [ToolCall(id="1", name="crash", arguments="{}")],
        {"crash": crash},
    )
    assert json.loads(r.output) == {"success": False, "code": "internal", "error": "kaboom"}


@pytest.mark.asyncio
async def test_safe_tool_honors_code_hint_on_non_tool_error():
    @safe_tool
    def conflict() -> str:
        e = RuntimeError("write conflict")
        e.code = "conflict"
        e.hint = "re-read before write"
        raise e

    [r] = await execute_tools(
        [ToolCall(id="1", name="conflict", arguments="{}")],
        {"conflict": conflict},
    )
    assert json.loads(r.output) == {
        "success": False, "code": "conflict",
        "error": "write conflict", "hint": "re-read before write",
    }


# ── execution-plane error-aware serialization ─────────────────────────────────

@pytest.mark.asyncio
async def test_plane_returns_clean_message_for_classic_throw():
    @tool
    def bad() -> str:
        raise RuntimeError("disk full")

    plane = LocalExecutionPlane().register(bad)
    results = [
        e async for e in plane._execute_single(
            ToolCall(id="1", name="bad", arguments="{}"), RunContext(),
        )
    ]
    tool_result = next(e for e in results if isinstance(e, ToolResultEvent))
    assert tool_result.is_error is True
    assert tool_result.content == "disk full"


@pytest.mark.asyncio
async def test_plane_emits_json_for_coded_throw():
    @tool
    def coded() -> str:
        e = RuntimeError("nothing matched")
        e.code = "not_found"
        raise e

    plane = LocalExecutionPlane().register(coded)
    results = [
        e async for e in plane._execute_single(
            ToolCall(id="1", name="coded", arguments="{}"), RunContext(),
        )
    ]
    tool_result = next(e for e in results if isinstance(e, ToolResultEvent))
    assert tool_result.is_error is True
    parsed = json.loads(tool_result.content)
    assert parsed["message"] == "nothing matched"
    assert parsed["code"] == "not_found"


# ── ctx.audit best-effort ──────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_ctx_audit_failure_does_not_flip_is_error():
    @tool
    async def module_write(path: str, ctx=None) -> str:
        def boom():
            raise RuntimeError("audit store down")
        if ctx is not None and ctx.audit is not None:
            await ctx.audit("record-patch", boom)
        return json.dumps({"written": path})

    plane = LocalExecutionPlane().register(module_write)
    results = [
        e async for e in plane._execute_single(
            ToolCall(id="1", name="module_write", arguments=json.dumps({"path": "/x"})),
            RunContext(),
        )
    ]
    audit = next((e for e in results if isinstance(e, ToolAuditFailedEvent)), None)
    result = next((e for e in results if isinstance(e, ToolResultEvent)), None)
    assert audit is not None
    assert audit.label == "record-patch"
    assert audit.error == "audit store down"
    assert result is not None
    assert result.is_error is False  # foot-gun fixed: already-committed write reported as success
    assert json.loads(result.content) == {"written": "/x"}


@pytest.mark.asyncio
async def test_ctx_audit_failures_flush_on_subsequent_throw():
    @tool
    async def partial(ctx=None) -> str:
        def boom():
            raise RuntimeError("metric collector down")
        if ctx is not None and ctx.audit is not None:
            await ctx.audit("metric", boom)
        raise RuntimeError("then main work failed")

    plane = LocalExecutionPlane().register(partial)
    results = [
        e async for e in plane._execute_single(
            ToolCall(id="1", name="partial", arguments="{}"),
            RunContext(),
        )
    ]
    assert any(
        isinstance(e, ToolAuditFailedEvent) and e.label == "metric"
        for e in results
    )
    r = next(e for e in results if isinstance(e, ToolResultEvent))
    assert r.is_error is True
    assert r.content == "then main work failed"


# ── streaming-tool throw convention ────────────────────────────────────────────

@pytest.mark.asyncio
async def test_streaming_throw_produces_clean_tool_result():
    @streaming_tool
    async def stream_fail():
        yield "starting..."
        raise RuntimeError("midway crash")

    plane = LocalExecutionPlane().register(stream_fail)
    results = [
        e async for e in plane._execute_single(
            ToolCall(id="1", name="stream_fail", arguments="{}"),
            RunContext(),
        )
    ]
    r = next(e for e in results if isinstance(e, ToolResultEvent))
    assert r.is_error is True
    assert r.content == "midway crash"


@pytest.mark.asyncio
async def test_streaming_failure_shaped_chunk_emits_warning():
    @streaming_tool
    async def legacy_stream():
        yield json.dumps({"success": False, "code": "not_found", "error": "x"})

    # Reset module-level warned set so warning fires for this tool name
    from deepstrike.runtime import execution_plane as ep
    ep._WARNED_FAILURE_SHAPES.discard("legacy_stream")

    plane = LocalExecutionPlane().register(legacy_stream)
    with warnings.catch_warnings(record=True) as warn_list:
        warnings.simplefilter("always")
        results = [
            e async for e in plane._execute_single(
                ToolCall(id="1", name="legacy_stream", arguments="{}"),
                RunContext(),
            )
        ]
    r = next(e for e in results if isinstance(e, ToolResultEvent))
    # The chunk is NOT auto-converted to is_error (the foot-gun the warning calls out)
    assert r.is_error is False
    msgs = [str(w.message) for w in warn_list]
    assert any('streaming tool "legacy_stream" yielded a failure-shaped chunk' in m for m in msgs)
