"""
11 — system_prompt, initialMemory, saveSession, knowledge.init(), frontmatter, AttemptLoop streaming
"""
import pytest
from deepstrike import (
    AttemptLoop, AttemptRequest, Criterion, LlmEvalJudge, RuntimeAttemptBody, StopPolicy,
)
from conftest import make_agent, make_provider, collect_events, text, MockDreamStore, MockKnowledgeSource, SKILL_DIR


# ─── system_prompt ────────────────────────────────────────────────────────

class TestSystemPrompt:
    @pytest.mark.timeout(60)
    async def test_agent_follows_system_prompt(self):
        result = await make_agent(
            system_prompt="You are a pirate. Always end every reply with 'Arrr!'"
        ).run("Say hello.")
        assert "arrr" in result.lower(), f"expected 'Arrr!' in: {result}"


# ─── initialMemory ────────────────────────────────────────────────────────

class TestInitialMemory:
    @pytest.mark.timeout(60)
    async def test_agent_recalls_preseeded_memory(self):
        result = await make_agent(
            initial_memory=["The user's favourite colour is chartreuse."]
        ).run("What is the user's favourite colour? Answer in one word.")
        assert "chartreuse" in result.lower(), f"expected 'chartreuse' in: {result}"


# ─── saveSession ──────────────────────────────────────────────────────────

class TestSaveSession:
    @pytest.mark.timeout(60)
    async def test_save_session_called_after_run(self):
        store = MockDreamStore()
        await make_agent(dream_store=store, agent_id="test-agent").run('Reply "ok".')
        assert len(store.saved_sessions) >= 1, "save_session should have been called"
        assert store.saved_sessions[0].agent_id == "test-agent"


# ─── KnowledgeSource.init() ───────────────────────────────────────────────

class TestKnowledgeInit:
    @pytest.mark.timeout(60)
    async def test_init_called_before_run(self):
        ks = MockKnowledgeSource(["DeepStrike is a Rust-kernel agent framework."])
        await make_agent(knowledge_source=ks).run('Reply "ok".')
        assert ks.init_called >= 1, "init() should have been called"

    @pytest.mark.timeout(60)
    async def test_init_called_once_per_run(self):
        ks = MockKnowledgeSource([])
        await make_agent(knowledge_source=ks).run('Reply "ok".')
        assert ks.init_called == 1


# ─── Frontmatter stripping ────────────────────────────────────────────────

class TestFrontmatterStripping:
    @pytest.mark.timeout(60)
    async def test_skill_body_has_no_frontmatter(self):
        events = await collect_events(
            make_agent(skill_dir=SKILL_DIR).run_streaming(
                "Use the summarize skill on: 'Rust is fast, safe, and concurrent.' Then output the summary."
            )
        )
        output = text(events)
        assert "name: summarize" not in output, f"frontmatter leaked: {output}"
        assert len(output) > 0


# ─── AttemptLoop.stream() ────────────────────────────────────────────────

class TestAttemptLoopStreaming:
    @pytest.mark.timeout(90)
    async def test_emits_token_supervising_terminal(self):
        events = []
        result = ""
        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=LlmEvalJudge(make_provider()),
            stop=StopPolicy(max_attempts=2),
        )
        async for evt in loop.stream(AttemptRequest(
            goal="What is 6 * 7? Output only the number.",
            criteria=[Criterion(text="Answer must be 42")],
        )):
            events.append(evt)
            if evt.type == "token":
                result += str(evt.progress.payload.get("text", "")) if evt.progress else ""
        assert len(result) > 0
        assert any(e.type == "judging" for e in events)
        assert any(e.type == "completed" for e in events)

    @pytest.mark.timeout(90)
    async def test_done_verdict_has_details(self):
        verdict = None
        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=LlmEvalJudge(make_provider()),
            stop=StopPolicy(max_attempts=2),
        )
        async for evt in loop.stream(AttemptRequest(
            goal="Output the number 99.",
            criteria=[
                Criterion(text="Response must contain 99", required=True),
                Criterion(text="Response should be concise", required=False, weight=0.5),
            ],
        )):
            if evt.type == "completed" and evt.outcome:
                verdict = evt.outcome.verdict
        if verdict is not None:
            assert isinstance(verdict.passed, bool)
            assert isinstance(verdict.overall_score, float)
            assert isinstance(verdict.details, list)
