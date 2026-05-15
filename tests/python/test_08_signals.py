"""
08 — SignalGateway offline + Agent interrupt via signal
"""
import asyncio
import time
import pytest

from deepstrike import SignalGateway, ScheduledPrompt, RuntimeSignal
from deepstrike.providers.stream import DoneEvent

from conftest import make_agent, collect_events


class TestSignalGateway:
    def test_on_signal_fires_on_ingest(self):
        gw = SignalGateway()
        received = []
        gw.on_signal(lambda sig: received.append(sig.kind))
        gw.ingest(RuntimeSignal(kind="external"))
        gw.ingest(RuntimeSignal(kind="interrupt"))
        assert received == ["external", "interrupt"]

    async def test_schedule_fires_at_run_at_ms(self):
        gw = SignalGateway()
        received = []
        gw.on_signal(lambda sig: received.append(sig.payload.get("goal", "")))
        gw.schedule(ScheduledPrompt("fire-me", int(time.time() * 1000) + 40))
        await asyncio.sleep(0.1)
        assert "fire-me" in received
        gw.destroy()

    async def test_schedule_preserves_kernel_routing_metadata(self):
        gw = SignalGateway()
        run_at = int(time.time() * 1000) + 40
        gw.schedule(ScheduledPrompt("fire-me", run_at))
        await asyncio.sleep(0.1)
        sig = await gw.next_signal()
        assert sig is not None
        assert sig.source == "cron"
        assert sig.signal_type == "job"
        assert sig.urgency == "normal"
        assert sig.dedupe_key == f"cron:fire-me:{run_at}"
        gw.destroy()

    async def test_cancel_prevents_firing(self):
        gw = SignalGateway()
        run_at = int(time.time() * 1000) + 50
        gw.schedule(ScheduledPrompt("test", run_at))
        gw.cancel("test", run_at)
        await asyncio.sleep(0.1)
        received = []
        gw.on_signal(lambda sig: received.append(1))
        assert len(received) == 0
        gw.destroy()

    async def test_schedule_idempotent(self):
        gw = SignalGateway()
        received = []
        gw.on_signal(lambda _: received.append(1))
        run_at = int(time.time() * 1000) + 40
        gw.schedule(ScheduledPrompt("once", run_at))
        gw.schedule(ScheduledPrompt("once", run_at))
        await asyncio.sleep(0.1)
        assert len(received) == 1
        gw.destroy()

    async def test_destroy_clears_pending(self):
        gw = SignalGateway()
        received = []
        gw.on_signal(lambda _: received.append(1))
        gw.schedule(ScheduledPrompt("never", int(time.time() * 1000) + 200))
        gw.destroy()
        await asyncio.sleep(0.25)
        assert len(received) == 0


class TestAgentInterruptViaSignal:
    @pytest.mark.timeout(60)
    async def test_interrupt_stops_run(self):
        agent = make_agent(max_turns=50)
        events = []
        async for evt in agent.run_streaming("Count from 1 to 10000."):
            events.append(evt)
            if not agent._interrupted:
                agent.interrupt()

        assert any(isinstance(e, DoneEvent) for e in events), "done must be emitted"
