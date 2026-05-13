from __future__ import annotations
import json
import re
import time
import random
import string
from datetime import datetime
from pathlib import Path
from typing import Optional
from meetingmind.paths import MEETINGS_DIR, ACTIONS_DIR
from meetingmind.types import MeetingRecord, ActionItem, Decision


def generate_id(prefix: str = "") -> str:
    now = datetime.now()
    date = now.strftime("%Y%m%d")
    t = now.strftime("%H%M%S")
    rand = "".join(random.choices(string.ascii_lowercase + string.digits, k=6))
    return f"{prefix}{date}_{t}_{rand}"


# ─── meetings ────────────────────────────────────────────────────────────────

async def save_meeting(meeting: MeetingRecord) -> None:
    MEETINGS_DIR.mkdir(parents=True, exist_ok=True)
    path = MEETINGS_DIR / f"{meeting['id']}.json"
    path.write_text(json.dumps(meeting, indent=2, ensure_ascii=False))


async def load_meetings(limit: int = 100) -> list[MeetingRecord]:
    try:
        files = sorted(MEETINGS_DIR.glob("*.json"))[-limit:]
        meetings: list[MeetingRecord] = []
        for f in files:
            try:
                meetings.append(json.loads(f.read_text()))
            except Exception:
                pass
        return sorted(meetings, key=lambda m: m.get("created_at", 0), reverse=True)
    except Exception:
        return []


async def load_meeting(meeting_id: str) -> Optional[MeetingRecord]:
    path = MEETINGS_DIR / f"{meeting_id}.json"
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text())
    except Exception:
        return None


# ─── action items ─────────────────────────────────────────────────────────────

async def save_action(action: ActionItem) -> None:
    ACTIONS_DIR.mkdir(parents=True, exist_ok=True)
    path = ACTIONS_DIR / f"{action['id']}.json"
    path.write_text(json.dumps(action, indent=2, ensure_ascii=False))


async def load_actions(status: Optional[str] = None, project: Optional[str] = None) -> list[ActionItem]:
    try:
        files = sorted(ACTIONS_DIR.glob("*.json"))
        actions: list[ActionItem] = []
        for f in files:
            try:
                a: ActionItem = json.loads(f.read_text())
                if status and a.get("status") != status:
                    continue
                actions.append(a)
            except Exception:
                pass

        if project:
            meeting_ids = {
                m["id"] for m in await load_meetings()
                if m.get("project", "").lower() == project.lower()
            }
            actions = [a for a in actions if a.get("meeting_id") in meeting_ids]

        return sorted(actions, key=lambda a: a.get("created_at", 0), reverse=True)
    except Exception:
        return []


async def update_action_status(action_id: str, status: str) -> bool:
    path = ACTIONS_DIR / f"{action_id}.json"
    if not path.exists():
        return False
    try:
        action: ActionItem = json.loads(path.read_text())
        action["status"] = status  # type: ignore[typeddict-item]
        path.write_text(json.dumps(action, indent=2, ensure_ascii=False))
        return True
    except Exception:
        return False


# ─── parse agent output ───────────────────────────────────────────────────────

def parse_meeting_output(
    text: str,
    transcript: str,
    title: str = "",
    project: str = "",
    participants: Optional[list[str]] = None,
) -> Optional[MeetingRecord]:
    match = re.search(r"```json\s*([\s\S]*?)```", text) or re.search(r"\{[\s\S]*\"summary\"[\s\S]*\}", text)
    if not match:
        return None
    json_str = match.group(1) if match.lastindex else match.group(0)
    try:
        parsed = json.loads(json_str)
        if not parsed.get("summary"):
            return None
        mid = generate_id("mtg_")
        actions: list[ActionItem] = []
        for raw_a in parsed.get("actions", []):
            actions.append({
                "id": generate_id("act_"),
                "content": str(raw_a.get("content", "")),
                "owner": str(raw_a.get("owner", "")),
                "due_date": str(raw_a.get("due_date", "")),
                "status": "open",
                "meeting_id": mid,
                "created_at": int(time.time() * 1000),
            })
        decisions: list[Decision] = [
            {"content": str(d.get("content", "")), "made_by": d.get("made_by", []), "meeting_id": mid}
            for d in parsed.get("decisions", [])
        ]
        return {
            "id": mid,
            "title": title or parsed.get("title", f"Meeting {datetime.now().strftime('%Y-%m-%d')}"),
            "date": datetime.now().strftime("%Y-%m-%d"),
            "participants": participants or parsed.get("participants", []),
            "project": project or parsed.get("project", ""),
            "transcript": transcript,
            "summary": parsed["summary"],
            "actions": actions,
            "decisions": decisions,
            "blockers": parsed.get("blockers", []),
            "tags": parsed.get("tags", []),
            "created_at": int(time.time() * 1000),
        }
    except Exception:
        return None
