import json
from pathlib import Path

import pytest

from deepstrike.runtime.os_snapshot import (
    rebuild_os_snapshot_from_session_events,
    session_log_has_required_categories,
)

FIXTURES = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "session"


def _load(name: str):
    return json.loads((FIXTURES / name).read_text())


@pytest.mark.parametrize(
    ("events_file", "snapshot_file"),
    [
        ("events_spawn_lifecycle.json", "os_snapshot_spawn_lifecycle.json"),
        ("events_ask_user.json", "os_snapshot_ask_user.json"),
    ],
)
def test_os_snapshot_golden_fixtures(events_file: str, snapshot_file: str):
    events = _load(events_file)
    assert session_log_has_required_categories(events)
    snap = rebuild_os_snapshot_from_session_events(events)
    expected = _load(snapshot_file)

    assert snap.last_suspend == expected.get("last_suspend")
    assert snap.last_resumed_turn == expected.get("last_resumed_turn")
    assert snap.page_out_count == expected["page_out_count"]
    assert snap.page_in_count == expected["page_in_count"]
    assert snap.tool_gated_count == expected["tool_gated_count"]
    assert len(snap.process_by_agent) == len(expected["process_by_agent"])
    assert len(snap.signals) == len(expected["signals"])
