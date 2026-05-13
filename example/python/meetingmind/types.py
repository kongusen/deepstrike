from __future__ import annotations
from typing import TypedDict, Literal

ActionStatus = Literal["open", "done", "blocked"]


class ActionItem(TypedDict, total=False):
    id: str
    content: str
    owner: str
    due_date: str
    status: ActionStatus
    meeting_id: str
    created_at: int


class Decision(TypedDict):
    content: str
    made_by: list[str]
    meeting_id: str


class MeetingRecord(TypedDict, total=False):
    id: str
    title: str
    date: str
    participants: list[str]
    project: str
    transcript: str
    summary: str
    actions: list[ActionItem]
    decisions: list[Decision]
    blockers: list[str]
    tags: list[str]
    created_at: int
