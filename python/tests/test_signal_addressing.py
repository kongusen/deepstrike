"""R1 / L0 — recipient addressing on a shared SignalGateway (python parity with the node test).

One gateway serves N peer loops: each pulls with its own session_id and drains only signals
addressed to it (plus unaddressed broadcasts); other recipients' signals stay queued. Omitting
the recipient preserves the legacy FIFO behaviour.
"""

import pytest

from deepstrike import RuntimeSignal, SignalGateway


def _sig(summary: str, recipient: str | None = None) -> RuntimeSignal:
    return RuntimeSignal(
        kind="external",
        payload={"goal": summary},
        source="gateway",
        recipient=recipient,
    )


@pytest.mark.asyncio
async def test_each_loop_drains_own_plus_shared():
    gw = SignalGateway()
    gw.ingest(_sig("to-a", "sess-a"))
    gw.ingest(_sig("to-b", "sess-b"))
    gw.ingest(_sig("shared"))

    a1 = await gw.next_signal("sess-a")
    a2 = await gw.next_signal("sess-a")
    assert sorted([a1.payload["goal"], a2.payload["goal"]]) == ["shared", "to-a"]
    assert await gw.next_signal("sess-a") is None

    # sess-b's signal still queued for its own puller.
    b = await gw.next_signal("sess-b")
    assert b.payload["goal"] == "to-b"


@pytest.mark.asyncio
async def test_preserves_fifo_among_visible():
    gw = SignalGateway()
    gw.ingest(_sig("first", "sess-a"))
    gw.ingest(_sig("to-b", "sess-b"))
    gw.ingest(_sig("second"))  # broadcast, after to-b
    assert (await gw.next_signal("sess-a")).payload["goal"] == "first"
    assert (await gw.next_signal("sess-a")).payload["goal"] == "second"


@pytest.mark.asyncio
async def test_omitting_recipient_is_legacy_fifo():
    gw = SignalGateway()
    gw.ingest(_sig("x", "sess-a"))
    gw.ingest(_sig("y"))
    assert (await gw.next_signal()).payload["goal"] == "x"
    assert (await gw.next_signal()).payload["goal"] == "y"
    assert await gw.next_signal() is None


@pytest.mark.asyncio
async def test_broadcast_fans_out_to_explicit_recipients():
    gw = SignalGateway()
    gw.broadcast(["sess-a", "sess-b"], _sig("all"))

    assert (await gw.next_signal("sess-a")).payload["goal"] == "all"
    assert (await gw.next_signal("sess-b")).payload["goal"] == "all"


def test_observer_failure_does_not_turn_committed_ingest_into_failure():
    failures = []
    gw = SignalGateway(on_observer_error=lambda failure: failures.append(failure))
    gw.on_signal(lambda _signal: (_ for _ in ()).throw(RuntimeError("observer unavailable")))

    gw.ingest(_sig("committed"))

    assert gw.depth == 1
    assert failures[0].operation == "signal_listener"
