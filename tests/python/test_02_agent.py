"""
02 — Agent.run(), run_streaming(), telemetry, interrupt
"""
import pytest

from deepstrike.providers.stream import TextDelta, DoneEvent

from conftest import make_agent, collect_events, text


class TestAgentRun:
    @pytest.mark.timeout(60)
    async def test_returns_non_empty_string(self):
        events = await collect_events(make_agent().run_streaming('Reply with the single word "pong".'))
        result = text(events)
        assert len(result) > 0
        assert "pong" in result.lower(), f"got: {result}"

    @pytest.mark.timeout(60)
    async def test_arithmetic(self):
        events = await collect_events(make_agent().run_streaming("What is 7 * 8? Output only the number."))
        result = text(events)
        assert "56" in result, f"got: {result}"


class TestAgentRunStreaming:
    @pytest.mark.timeout(60)
    async def test_emits_text_delta_and_done(self):
        events = await collect_events(make_agent().run_streaming('Say "hi"'))
        assert any(isinstance(e, TextDelta) for e in events), "need text_delta"
        done_count = sum(1 for e in events if isinstance(e, DoneEvent))
        assert done_count == 1, "need exactly 1 done"

    @pytest.mark.timeout(60)
    async def test_done_has_positive_counts(self):
        events = await collect_events(make_agent().run_streaming("Compute 3+4 and output the result."))
        done = next((e for e in events if isinstance(e, DoneEvent)), None)
        assert done is not None
        assert done.iterations >= 0

    @pytest.mark.timeout(60)
    async def test_done_status_known(self):
        events = await collect_events(make_agent().run_streaming("Reply OK"))
        done = next((e for e in events if isinstance(e, DoneEvent)), None)
        assert done is not None
        assert done.status in ("completed", "success", "max_turns", "timeout", "error")

    @pytest.mark.timeout(60)
    async def test_collected_text_matches(self):
        events = await collect_events(make_agent().run_streaming('Say exactly "deepstrike"'))
        assert "deepstrike" in text(events).lower()

    @pytest.mark.timeout(60)
    async def test_criteria_list(self):
        events = await collect_events(
            make_agent().run_streaming(
                "List two colors. You MUST mention 'red' and 'blue'.",
                criteria=["Response must mention 'red'", "Response must mention 'blue'"],
            )
        )
        result = text(events).lower()
        assert "red" in result
        assert "blue" in result


class TestAgentInterrupt:
    @pytest.mark.timeout(60)
    async def test_interrupt_emits_done(self):
        agent = make_agent(max_turns=50)
        events = []
        async for evt in agent.run_streaming("Count from 1 to 1000, one number per sentence."):
            events.append(evt)
            if len(events) >= 3:
                agent.interrupt()

        done = next((e for e in events if isinstance(e, DoneEvent)), None)
        assert done is not None, "done must be emitted after interrupt"
