"""
04 — SkillRegistry + Agent with skillDir
"""
import pytest

from deepstrike import SkillRegistry

from conftest import make_agent, SKILL_DIR


class TestSkillRegistry:
    def test_scan_returns_metadata(self):
        registry = SkillRegistry(SKILL_DIR)
        metas = registry.scan()
        assert len(metas) >= 2, f"got {len(metas)} skills"
        names = [m.name for m in metas]
        assert "summarize" in names
        assert "count_words" in names

    def test_each_entry_has_name_and_description(self):
        for m in SkillRegistry(SKILL_DIR).scan():
            assert len(m.name) > 0
            assert len(m.description) > 0

    def test_parses_optional_frontmatter(self):
        metas = SkillRegistry(SKILL_DIR).scan()
        s = next((m for m in metas if m.name == "summarize"), None)
        assert s is not None
        assert s.when_to_use and len(s.when_to_use) > 0
        assert s.effort is not None

    def test_returns_empty_for_nonexistent_dir(self):
        metas = SkillRegistry("/tmp/no-such-skills-xyz").scan()
        assert metas == []


class TestAgentWithSkillDir:
    @pytest.mark.timeout(120)
    async def test_agent_produces_summary(self):
        agent = make_agent(skill_dir=SKILL_DIR)
        result = await agent.run(
            "Use the summarize skill to summarize: "
            "'DeepStrike is a Rust-based AI agent framework with a pure-computation kernel "
            "and bindings for Node.js, Python, and Rust.'"
        )
        assert len(result) > 0
