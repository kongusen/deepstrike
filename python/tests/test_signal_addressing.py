"""R1 / L0 — recipient addressing on a shared SignalGateway (python parity with the node test).

One gateway serves N peer loops: each claims with its own session_id and drains only signals
addressed to it (plus unaddressed broadcasts); other recipients' signals stay queued. Omitting
the recipient claims from the shared FIFO.
"""

import pytest

from deepstrike import RuntimeSignal, SignalGateway


def _sig(summary: str, recipient: str | None = None) -> RuntimeSignal:
    return RuntimeSignal(
        source="gateway",
        signal_type="event",
        urgency="normal",
        payload={"goal": summary},
        recipient=recipient,
    )


async def _claim_and_ack(gateway: SignalGateway, recipient: str | None = None) -> RuntimeSignal | None:
    claim = await gateway.claim_signal(recipient)
    if claim is None:
        return None
    assert await gateway.ack_signal(claim) is True
    return claim.signal


@pytest.mark.asyncio
async def test_unacked_claim_is_redelivered_after_lease_expiry():
    now = 1_000
    gw = SignalGateway(now=lambda: now, default_lease_ms=100)
    gw.ingest(_sig("leased", "sess-a"))

    first = await gw.claim_signal("sess-a")
    assert first.signal.payload["goal"] == "leased"
    assert await gw.claim_signal("sess-a") is None

    now += 101
    second = await gw.claim_signal("sess-a")
    assert second.signal.payload["goal"] == "leased"
    assert second.lease_token != first.lease_token
    assert second.delivery_attempt == 2
    assert await gw.ack_signal(first) is False
    assert await gw.ack_signal(second) is True
    assert gw.depth == 0


@pytest.mark.asyncio
async def test_nacked_claim_is_immediately_available_for_redelivery():
    gw = SignalGateway()
    gw.ingest(_sig("retry", "sess-a"))

    first = await gw.claim_signal("sess-a")
    assert await gw.nack_signal(first) is True
    second = await gw.claim_signal("sess-a")

    assert second.signal.payload["goal"] == "retry"
    assert second.lease_token != first.lease_token


@pytest.mark.asyncio
async def test_each_loop_drains_own_plus_shared():
    gw = SignalGateway()
    gw.ingest(_sig("to-a", "sess-a"))
    gw.ingest(_sig("to-b", "sess-b"))
    gw.ingest(_sig("shared"))

    a1 = await _claim_and_ack(gw, "sess-a")
    a2 = await _claim_and_ack(gw, "sess-a")
    assert sorted([a1.payload["goal"], a2.payload["goal"]]) == ["shared", "to-a"]
    assert await _claim_and_ack(gw, "sess-a") is None

    # sess-b's signal still queued for its own puller.
    b = await _claim_and_ack(gw, "sess-b")
    assert b.payload["goal"] == "to-b"


@pytest.mark.asyncio
async def test_preserves_fifo_among_visible():
    gw = SignalGateway()
    gw.ingest(_sig("first", "sess-a"))
    gw.ingest(_sig("to-b", "sess-b"))
    gw.ingest(_sig("second"))  # broadcast, after to-b
    assert (await _claim_and_ack(gw, "sess-a")).payload["goal"] == "first"
    assert (await _claim_and_ack(gw, "sess-a")).payload["goal"] == "second"


@pytest.mark.asyncio
async def test_omitting_recipient_claims_shared_fifo():
    gw = SignalGateway()
    gw.ingest(_sig("x", "sess-a"))
    gw.ingest(_sig("y"))
    assert (await _claim_and_ack(gw)).payload["goal"] == "x"
    assert (await _claim_and_ack(gw)).payload["goal"] == "y"
    assert await _claim_and_ack(gw) is None


@pytest.mark.asyncio
async def test_broadcast_fans_out_to_explicit_recipients():
    gw = SignalGateway()
    gw.broadcast(["sess-a", "sess-b"], _sig("all"))

    assert (await _claim_and_ack(gw, "sess-a")).payload["goal"] == "all"
    assert (await _claim_and_ack(gw, "sess-b")).payload["goal"] == "all"


def test_observer_failure_does_not_turn_committed_ingest_into_failure():
    failures = []
    gw = SignalGateway(on_observer_error=lambda failure: failures.append(failure))
    gw.on_signal(lambda _signal: (_ for _ in ()).throw(RuntimeError("observer unavailable")))

    gw.ingest(_sig("committed"))

    assert gw.depth == 1
    assert failures[0].operation == "signal_listener"
