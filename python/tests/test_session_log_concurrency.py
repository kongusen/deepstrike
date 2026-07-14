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
