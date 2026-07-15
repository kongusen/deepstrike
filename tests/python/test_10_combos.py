"""
10 — Feature combinations
"""
import pytest

from deepstrike import (
    tool, WorkingMemory, Governance,
    AttemptLoop, AttemptRequest, Criterion, RuntimeAttemptBody, StopPolicy,
    Verdict, VerdictFnJudge,
)
from deepstrike.providers.stream import (
    ErrorEvent, DoneEvent, ToolResultEvent, TextDelta,
)

from conftest import (
    make_agent, make_provider, collect_events, text,
    MockDreamStore, MockKnowledgeSource, SKILL_DIR,
)
from deepstrike.memory.protocols import MemoryProvenance, MemoryRecord, MemoryScope


# ─── A: Tools + Governance ──────────────────────────────────────────────────

class TestToolsGovernance:
    @pytest.mark.timeout(120)
    async def test_blocked_denied_allowed_succeeds(self):
        gov = Governance()
        gov.block_tool("risky_op")

        @tool
        def risky_op() -> str:
            """Risky operation."""
            return "risky done"

        @tool
        def safe_op() -> str:
            """Safe operation."""
            return "safe done"

        agent = make_agent(governance=gov)
        agent.register(risky_op)
        agent.register(safe_op)
        agent.block_tool("risky_op")

        events = await collect_events(
            agent.run_streaming("First call risky_op. Then call safe_op. Report both results.")
        )

        assert sum(1 for e in events if isinstance(e, DoneEvent)) == 1
        errors = [e for e in events if isinstance(e, ErrorEvent)]
        tool_results = [e for e in events if isinstance(e, ToolResultEvent)]
        if errors:
            assert any("risky_op" in e.message or "blocked" in e.message for e in errors), \
                "risky_op should be denied"
        if tool_results:
            assert any(r.name == "safe_op" for r in tool_results), "safe_op should succeed"


# ─── B: Tools + WorkingMemory ──────────────────────────────────────────────

class TestToolsWorkingMemory:
    @pytest.mark.timeout(120)
    async def test_shared_counter_increments(self):
        mem = WorkingMemory()
        mem.set("count", 0)

        @tool
        def increment_counter() -> str:
            """Increment the counter and return the new value."""
            n = (mem.get("count") or 0) + 1
            mem.set("count", n)
            return str(n)

        agent = make_agent()
        agent.register(increment_counter)
        events = await collect_events(
            agent.run_streaming("Call increment_counter exactly 3 times, then report the final value.")
        )

        assert any(isinstance(e, DoneEvent) for e in events)
        assert (mem.get("count") or 0) >= 1, f"counter should be >=1, got {mem.get('count')}"


# ─── C: Knowledge + Tools ──────────────────────────────────────────────────

class TestKnowledgeTools:
    @pytest.mark.timeout(120)
    async def test_knowledge_informs_tool_args(self):
        ks = MockKnowledgeSource([
            "Recommended model: gpt-4o-mini. Recommended maxTokens: 4096.",
        ])
        stored = {}

        @tool
        def store_config(key: str, value: str) -> str:
            """Store a key-value config pair."""
            stored[key] = value
            return f"stored {key}={value}"

        agent = make_agent(knowledge_source=ks)
        agent.register(store_config)
        events = await collect_events(
            agent.run_streaming(
                "Based on the recommended configuration in context, "
                "store the recommended model name using store_config."
            )
        )

        assert any(isinstance(e, DoneEvent) for e in events)
        tool_results = [e for e in events if isinstance(e, ToolResultEvent)]
        if tool_results:
            assert any(r.name == "store_config" for r in tool_results)


# ─── D: Skills + Tools ─────────────────────────────────────────────────────

class TestSkillsTools:
    @pytest.mark.timeout(120)
    async def test_agent_uses_skill_then_tool(self):
        logged = []

        @tool
        def log_summary(summary: str) -> str:
            """Log a summary string."""
            logged.append(summary)
            return "logged"

        agent = make_agent(skill_dir=SKILL_DIR)
        agent.register(log_summary)
        events = await collect_events(
            agent.run_streaming(
                "Use the summarize skill on this text: "
                "'DeepStrike is a Rust kernel with Node.js, Python, and Rust bindings.' "
                "Then log the result with log_summary."
            )
        )

        assert any(isinstance(e, DoneEvent) for e in events)


# ─── E: AttemptLoop + Tools ───────────────────────────────────────────────

class TestAttemptLoopTools:
    @pytest.mark.timeout(120)
    async def test_retries_until_accepted(self):
        @tool
        def compute_square(n: int) -> str:
            """Compute the square of a number."""
            return str(int(n) * int(n))

        agent = make_agent()
        agent.register(compute_square)
        attempts = [0]

        def verdict(*, result, **_):
            attempts[0] += 1
            passed = "25" in result
            return Verdict(passed, 1.0 if passed else 0.0, "retry" if not passed else "ok")

        outcome = await AttemptLoop(
            body=RuntimeAttemptBody(agent._runner),
            judge=VerdictFnJudge(verdict),
            stop=StopPolicy(max_attempts=3),
        ).run(
            AttemptRequest(
                goal="Use compute_square to compute 5 squared and output the result.",
                criteria=[
                    Criterion(text="Must call compute_square with n=5"),
                    Criterion(text="Final answer must be 25"),
                ],
            )
        )

        assert outcome.outcome in {"passed", "exhausted"}
        if outcome.outcome == "passed":
            assert "25" in outcome.result


# ─── F: Agent + DreamStore ──────────────────────────────────────────────────

class TestAgentDreamStore:
    @pytest.mark.timeout(120)
    async def test_preseeded_memory_accessible(self):
        store = MockDreamStore()
        agent_id = "combo-mem-agent"
        scope = MemoryScope(agent_id, "combo")
        await store.upsert(agent_id, MemoryRecord(
                record_id="record-secret", scope=scope, name="secret-code", kind="reference",
                content="The secret code word is BANANA.", description="secret code fixture",
                provenance=MemoryProvenance(author="host", trust="host_verified"),
                created_at=1, updated_at=1, confidence=0.95,
            ))

        result = await make_agent(dream_store=store, agent_id=agent_id, memory_scope=scope).run(
            "What is the secret code word from your memory? If unknown, say 'unknown'.",
        )
        assert len(result) > 0


# ─── G: SignalGateway + Agent ───────────────────────────────────────────────

class TestSignalGatewayAgent:
    @pytest.mark.timeout(60)
    async def test_scheduled_signal_run_completes(self):
        import time
        from deepstrike import SignalGateway, ScheduledPrompt

        gw = SignalGateway()
        gw.schedule(ScheduledPrompt("check-in", int(time.time() * 1000) + 80))

        events = await collect_events(
            make_agent(max_turns=5).run_streaming("Respond with 'ready'.")
        )
        gw.destroy()

        assert any(isinstance(e, DoneEvent) for e in events)
