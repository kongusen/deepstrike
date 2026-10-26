from dataclasses import replace
import json

import pytest

from deepstrike import (
    InMemoryMemoryCandidateSink, InMemorySessionLog, LocalExecutionPlane,
    MemoryCandidate, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryRuntimePolicy, MemoryScope,
    RuntimeOptions, RuntimeRunner, collect_text, memory_candidate_id,
)
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool

OFF = MemoryRuntimePolicy("disabled", "disabled", "disabled", "disabled")
SCOPE = MemoryScope("agent", "candidates")
FACT = "RECALLED_FACT_FOR_POLICY"


def record():
    return MemoryRecord("r", SCOPE, "brief-style", "project", FACT, "format",
                        MemoryProvenance("extraction", "untrusted"), 1, 1)


class Store:
    def __init__(self):
        self.puts = []
        self.searches = 0
        self.sessions = []

    async def put(self, agent_id, memory):
        self.puts.append(memory)

    async def search(self, agent_id, query):
        self.searches += 1
        return [MemoryRecall(record(), .9, "fixture")]

    async def save_session(self, session):
        self.sessions.append(session)


class Provider:
    def __init__(self, text="done"):
        self.text = text
        self.calls = 0
        self.saw_recall = False

    async def complete(self, *args, **kwargs):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        self.saw_recall |= FACT in str(context)
        yield TextDelta(delta=self.text)


class ReceiptSink:
    def __init__(self, status, **fields):
        self.status, self.fields = status, fields

    async def submit(self, agent_id, candidate):
        return {"status": self.status, "candidate_id": candidate.candidate_id, "receipt_id": "receipt:host", **self.fields}


def runner_with(**kwargs):
    defaults = dict(provider=Provider(), session_log=InMemorySessionLog(), execution_plane=LocalExecutionPlane(),
                    max_tokens=1024, agent_id="agent", memory_scope=SCOPE, memory_store=Store())
    defaults.update(kwargs)
    return RuntimeRunner(RuntimeOptions(**defaults))


async def events(runner, session="session"):
    return [entry.event for entry in await runner._opts.session_log.read(session)]


@pytest.mark.parametrize("path", ["session_extraction", "model_writes", "semantic_archive"])
def test_candidate_path_requires_sink_at_construction(path):
    with pytest.raises(ValueError, match="needs RuntimeOptions.memory_candidate_sink"):
        runner_with(memory_runtime_policy=replace(OFF, **{path: "candidate_sink"}))


@pytest.mark.asyncio
@pytest.mark.parametrize("enabled", [False, True])
async def test_retrieval_gate_precedes_initial_and_renewal_hooks(enabled):
    phases = []
    def hook(goal, phase):
        phases.append(phase)
        return [MemoryQuery(SCOPE, goal)]
    runner = runner_with(memory_runtime_policy=replace(OFF, retrieval="host" if enabled else "disabled"), pre_query_memory=hook)
    await collect_text(runner.run(session_id="session", goal="facts"))
    await runner._memory_prefetch_queries("renewal")
    assert phases == (["initial", "renewal"] if enabled else [])
    assert bool(runner._opts.memory_store.searches) is enabled
    assert runner._opts.provider.saw_recall is enabled
    if not enabled:
        assert await runner.query_memory(MemoryQuery(SCOPE, "facts")) == []
        assert runner._opts.memory_store.searches == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["disabled", "candidate_sink", "legacy"])
async def test_session_extraction_routes_or_gates_before_auxiliary_provider(mode):
    aux = Provider(json.dumps({"memories": [
        {"name": "brief-style", "kind": "project", "content": "numbered sections", "description": "format"},
        {"name": "brief-length", "kind": "project", "content": "one page", "description": "length"},
    ]}))
    sink = InMemoryMemoryCandidateSink()
    runner = runner_with(memory_provider=aux, memory_candidate_sink=sink,
                         memory_runtime_policy=None if mode == "legacy" else replace(OFF, session_extraction=mode))
    await collect_text(runner.run(session_id="session", goal="say done"))
    assert len(runner._opts.memory_store.sessions) == 1
    assert aux.calls == (0 if mode == "disabled" else 1)
    assert len(runner._opts.memory_store.puts) == (2 if mode == "legacy" else 0)
    assert len(sink.candidates()) == (2 if mode == "candidate_sink" else 0)
    audit = await events(runner)
    assert len([e for e in audit if e["kind"] == "memory_written"]) == (2 if mode == "legacy" else 0)
    if mode == "candidate_sink":
        assert [c.candidate_id for c in sink.candidates()] == [memory_candidate_id(["session_extraction", "session", str(i)]) for i in range(2)]
        assert len([e for e in audit if e["kind"] == "memory_candidate_pending"]) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("status,fields,expected_kind,ok", [
    ("pending", {}, "memory_candidate_pending", True),
    ("accepted", {"record_ref": "host-record", "digest": "sha256:" + "0" * 64}, "memory_written", True),
    ("rejected", {"reason_code": "source_unverifiable"}, "memory_candidate_rejected", False),
])
async def test_host_write_funnel_audits_sink_answer(status, fields, expected_kind, ok):
    runner = runner_with(memory_candidate_sink=ReceiptSink(status, **fields))
    memory = record()
    assert await runner.write_memory(memory, session_id="session", candidate=MemoryCandidate("c1", "session_extraction", memory)) is ok
    assert runner._opts.memory_store.puts == []
    audit = await events(runner)
    assert len(audit) == 1 and audit[0]["kind"] == expected_kind
    if status == "accepted":
        assert audit[0]["record_id"] == "host-record"
    if status == "rejected":
        assert audit[0]["reason_code"] == "source_unverifiable"


@pytest.mark.asyncio
@pytest.mark.parametrize("fields", [{}, {"record_ref": "r"}, {"digest": "d"}])
async def test_accepted_receipt_requires_record_ref_and_digest(fields):
    runner = runner_with(memory_candidate_sink=ReceiptSink("accepted", **fields), memory_runtime_policy=replace(OFF, model_writes="candidate_sink"))
    result = await runner._persist_memory_effect_result("op", "e", {"name": "fact", "content": "durable"})
    assert "record_ref/digest" in result["error"]
    assert runner._opts.memory_store.puts == []


@pytest.mark.asyncio
async def test_model_effect_lost_ack_redelivery_conflict_and_distinct_effect():
    inner = InMemoryMemoryCandidateSink()
    class Lossy:
        drop = True
        async def submit(self, agent_id, candidate):
            receipt = await inner.submit(agent_id, candidate)
            if self.drop:
                self.drop = False
                raise RuntimeError("connection reset after storing")
            return receipt
    runner = runner_with(memory_candidate_sink=Lossy(), memory_runtime_policy=replace(OFF, model_writes="candidate_sink"))
    canonical = {"name": "fact", "content": "durable", "accepted_at_ms": 1}
    first = await runner._persist_memory_effect_result("op", "e", canonical)
    assert "connection reset" in first["error"]
    retry = await runner._persist_memory_effect_result("op", "e", canonical)
    assert retry["kind"] == "memory_candidate_result" and retry["status"] == "pending"
    assert retry["candidate_id"] == memory_candidate_id(["child_request", "op", "e", "0"])
    assert retry["receipt_id"] == "receipt:1"
    assert len(inner.candidates()) == 1
    conflict = await runner._persist_memory_effect_result("op", "e", {**canonical, "content": "different"}, session_id="session")
    assert conflict["reason_code"] == "candidate_conflict"
    assert (await events(runner))[0]["kind"] == "memory_candidate_rejected"
    other = await runner._persist_memory_effect_result("op", "e2", canonical)
    assert other["candidate_id"] != retry["candidate_id"]
    assert runner._opts.memory_store.puts == []


@pytest.mark.asyncio
@pytest.mark.parametrize("policy,puts", [(None, 1), (OFF, 0)])
async def test_model_effect_legacy_and_disabled(policy, puts):
    sink = InMemoryMemoryCandidateSink()
    runner = runner_with(memory_runtime_policy=policy, memory_candidate_sink=sink)
    result = await runner._persist_memory_effect_result("op", "e", {"name": "fact", "content": "durable"})
    assert result["kind"] == "memory_persist_result"
    assert len(runner._opts.memory_store.puts) == puts
    assert ("error" in result) == (puts == 0)
    assert sink.candidates() == []


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["disabled", "candidate_sink"])
async def test_real_semantic_page_out_routes_or_gates_before_summarizer(mode):
    calls = []
    class Fill(Provider):
        async def stream(self, context, tools, extensions=None, state=None):
            self.calls += 1
            if self.calls <= 8:
                yield ToolCallEvent(id=f"c{self.calls}", name="fill", arguments={})
            else:
                yield TextDelta(delta="done")
    @tool
    def fill() -> str:
        """Fill context."""
        return "w" * 200
    async def summarize(archived, context):
        calls.append(context)
        return "durable archive summary"
    sink = InMemoryMemoryCandidateSink()
    runner = runner_with(provider=Fill(), execution_plane=LocalExecutionPlane().register(fill),
                         baseline_tool_ids=["fill"], max_tokens=400, max_turns=20,
                         memory_runtime_policy=replace(OFF, semantic_archive=mode),
                         memory_candidate_sink=sink, memory_summarizer=summarize)
    await collect_text(runner.run(session_id="session", goal="fill until compact"))
    audit = await events(runner)
    assert any(e["kind"] == "compressed" for e in audit)
    assert runner._opts.memory_store.puts == []
    assert not any(e["kind"] == "memory_written" for e in audit)
    if mode == "disabled":
        assert calls == [] and sink.candidates() == []
        assert not any(e["kind"] == "semantic_archive_pending" for e in audit)
    else:
        assert calls and sink.candidates()
        assert not any(e["kind"] == "semantic_archive_completed" for e in audit)
        assert all(c.source == "semantic_archive" for c in sink.candidates())
        assert len([e for e in audit if e["kind"] == "memory_candidate_pending"]) == len(sink.candidates())


@pytest.mark.asyncio
async def test_archive_redelivery_uses_effect_identity():
    sink = InMemoryMemoryCandidateSink()
    runner = runner_with(memory_runtime_policy=replace(OFF, semantic_archive="candidate_sink"),
                         memory_candidate_sink=sink, memory_summarizer=lambda archived, ctx: "same summary")
    await runner._archive_semantic_page_out([], "auto_compact", "session", "effect:1")
    await runner._archive_semantic_page_out([], "auto_compact", "session", "effect:1")
    assert len(sink.candidates()) == 1
    pending = [e for e in await events(runner) if e["kind"] == "memory_candidate_pending"]
    assert pending[0]["receipt_id"] == pending[1]["receipt_id"]
    await runner._archive_semantic_page_out([], "auto_compact", "session", "effect:2")
    assert len(sink.candidates()) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("status", ["pending", "accepted", "rejected", "disabled"])
async def test_native_child_request_capability_and_candidate_settlement(status):
    """Real native PersistMemory -> runner sink -> existing bridge -> terminal audit."""
    from deepstrike.kernel.canonical import CanonicalKernel
    from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime
    from deepstrike.runtime.kernel_journal import InMemoryKernelJournal

    sink = InMemoryMemoryCandidateSink() if status == "pending" else ReceiptSink(
        status, **({"record_ref": "host:r", "digest": "sha256:" + "0" * 64} if status == "accepted"
                  else {"reason_code": "source_unverifiable"}),
    )
    runner = runner_with(memory_candidate_sink=sink, memory_runtime_policy=replace(
        OFF, model_writes="disabled" if status == "disabled" else "candidate_sink"))
    runtime = CanonicalRunnerRuntime(CanonicalKernel(), InMemoryKernelJournal(), "op-native", max_context_tokens=10000)
    await runtime.apply_host_event({"kind": "set_memory_enabled", "enabled": True, "read": False,
                                    "write": runner._memory_path_enabled("model_writes")})
    action = await runtime.start_workflow({"nodes": [
        {"task": {"goal": "remember formatting"}, "role": "explore"},
        {"task": {"goal": "keep workflow open"}, "role": "explore"},
    ]})
    assert action.kind == "spawn_workflow"
    node = action.nodes[0]
    await runtime.apply_host_event({"kind": "workflow_spawn_result", "effect_id": action.effect_id})
    action = await runtime._commit({"kind": "deliver_external_event", "event": {
        "kind": "child_completed", "task_id": node["agent_id"], "attempt_id": node["attempt_id"],
        "result": {"status": "completed", "usage": {"total_tokens": "0", "turns": 1}},
        "parent_requests": [{"kind": "request_memory_write", "proposal": {
            "name": "brief-style", "kind": "project", "content": "numbered sections", "description": "format", "evidence_refs": [],
        }}],
    }})
    if status == "disabled":
        assert action is None or action.kind != "persist_memory"
        assert any(o["kind"] == "control_request_rejected" and o.get("operation") == "write_memory"
                   for o in runtime.drain_host_observations())
        return
    assert action.kind == "persist_memory"
    result = await runner._persist_memory_effect_result(runtime.operation_id, action.effect_id, action.memory, session_id="session")
    settled = await runtime.apply_host_event(result)
    assert settled is None or settled.kind in ("done", "wait_sub_agents")
    observations = runtime.drain_host_observations()
    assert runner._opts.memory_store.puts == []
    assert any(o["kind"] == "memory_written" for o in observations) is (status == "accepted")
    if status == "pending":
        assert any(o["kind"] == "memory_candidate_pending" for o in observations)
        assert runtime.resume_action() is None or runtime.resume_action().kind != "persist_memory"
    if status == "rejected":
        assert any(e["kind"] == "memory_candidate_rejected" and e["reason_code"] == "source_unverifiable"
                   for e in await events(runner))


@pytest.mark.asyncio
@pytest.mark.parametrize("fields", [
    {"candidate_id": "wrong"}, {"receipt_id": ""}, {"status": "unknown"},
    {"status": "rejected", "reason_code": ""},
])
async def test_malformed_sink_receipt_never_becomes_a_write(fields):
    class Malformed:
        async def submit(self, agent_id, candidate):
            return {"status": "pending", "candidate_id": candidate.candidate_id, "receipt_id": "r", **fields}
    runner = runner_with(memory_candidate_sink=Malformed(), memory_runtime_policy=replace(OFF, model_writes="candidate_sink"))
    result = await runner._persist_memory_effect_result("op", "e", {"name": "fact", "content": "durable"})
    assert result["kind"] == "memory_candidate_result" and result.get("error")
    assert runner._opts.memory_store.puts == []


@pytest.mark.asyncio
async def test_semantic_archive_disabled_skips_fallback_provider_too():
    aux = Provider("archive")
    runner = runner_with(memory_runtime_policy=OFF, memory_provider=aux)
    await runner._archive_semantic_page_out([], "auto_compact", "session", "e")
    assert aux.calls == 0 and await events(runner) == []


def test_semantic_archive_projection_carries_metadata():
    from deepstrike.runtime.canonical_kernel_step import canonical_action_from_projection_json
    action = canonical_action_from_projection_json(json.dumps({
        "state": "action", "action": {
            "kind": "archive_page_out", "effect_id": "archive:7",
            "payload": {"payload": {"content": '[{"role":"user","content":"old turns"}]'}},
        },
    }), observations=[{"kind": "compressed", "action": "auto_compact", "summary": "old"}])
    assert action.kind == "archive_page_out"
    assert action.effect_id == "archive:7"
    assert action.action == "auto_compact" and action.tier == "semantic" and action.summary == "old"
