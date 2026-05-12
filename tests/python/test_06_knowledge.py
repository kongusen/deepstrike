"""
06 — KnowledgeSource (mock) + Agent with knowledge
"""
import pytest

from conftest import MockKnowledgeSource, make_agent, collect_events, text


class TestMockKnowledgeSource:
    async def test_retrieve_returns_all_when_top_k_large(self):
        ks = MockKnowledgeSource(["A", "B", "C"])
        assert await ks.retrieve("q", 10) == ["A", "B", "C"]

    async def test_retrieve_respects_top_k(self):
        ks = MockKnowledgeSource(["a", "b", "c", "d"])
        assert len(await ks.retrieve("q", 2)) == 2

    async def test_retrieve_empty_source(self):
        assert await MockKnowledgeSource([]).retrieve("q") == []


class TestAgentWithKnowledge:
    @pytest.mark.timeout(90)
    async def test_knowledge_snippets_influence_answer(self):
        ks = MockKnowledgeSource([
            "DeepStrike supports: OpenAI, Anthropic, Qwen, DeepSeek, MiniMax, Kimi, Ollama.",
        ])
        agent = make_agent(knowledge_source=ks)
        events = await collect_events(
            agent.run_streaming("List at least two LLM providers that DeepStrike supports.")
        )
        result = text(events).lower()
        providers = ["openai", "anthropic", "qwen", "deepseek", "minimax", "kimi", "ollama"]
        found = [p for p in providers if p in result]
        assert len(found) >= 2, f"expected >=2 providers, got: {result}"

    @pytest.mark.timeout(60)
    async def test_empty_knowledge_does_not_break(self):
        agent = make_agent(knowledge_source=MockKnowledgeSource([]))
        events = await collect_events(agent.run_streaming('Reply with just "ok".'))
        assert "ok" in text(events).lower()
