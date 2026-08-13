import asyncio

import pytest

from deepstrike.runtime.session_log import FileSessionLog


@pytest.mark.asyncio
async def test_file_session_log_serializes_concurrent_appends(tmp_path):
    log = FileSessionLog(tmp_path)

    returned = await asyncio.gather(
        log.append("sess-concurrent", {"kind": "run_started", "run_id": "r1", "goal": "a", "criteria": []}),
        log.append("sess-concurrent", {"kind": "run_started", "run_id": "r2", "goal": "b", "criteria": []}),
    )

    assert returned == [0, 1]
    assert [entry.seq for entry in await log.read("sess-concurrent")] == [0, 1]
    assert await log.latest_seq("sess-concurrent") == 1


@pytest.mark.asyncio
async def test_file_session_log_round_trips_prompt_measurement(tmp_path):
    log = FileSessionLog(tmp_path)
    await log.append("measurement", {
        "kind": "prompt_measured",
        "turn": 1,
        "measurement": {
            "version": 1,
            "request_fingerprint": "sha256:measurement",
            "input_tokens": 42,
            "source": {"kind": "heuristic"},
            "confidence": "low_confidence",
        },
    })

    entries = await FileSessionLog(tmp_path).read("measurement")
    assert entries[0].event["measurement"]["request_fingerprint"] == "sha256:measurement"
