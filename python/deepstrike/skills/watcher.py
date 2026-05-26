"""Skill directory watcher for Python SDK.

Uses ``watchfiles`` when available (Rust-backed, <1 ms latency) and falls back
to a pure-Python polling loop so there is no hard dependency.

Usage::

    async with watch_skill_dir(opts.skill_dir, on_changed) as watcher:
        await runner.run(goal)          # watcher fires in background
    # watcher stopped when context exits

Or imperatively::

    watcher = SkillWatcher(skill_dir, on_changed)
    task = asyncio.create_task(watcher.run())
    ...
    watcher.stop(); await task
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Callable, Coroutine
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import AsyncGenerator

_WATCHED_EXTS = {".md", ".json", ".py"}

OnChangedCallback = Callable[[str], Coroutine[Any, Any, None] | None]


class SkillWatcher:
    """Watches *skill_dir* and calls *on_changed(skill_dir)* on any relevant
    file-system event.  Debounces bursts with a 200 ms window.

    Prefers ``watchfiles`` if installed; otherwise polls every second.
    """

    def __init__(self, skill_dir: str, on_changed: OnChangedCallback) -> None:
        self._dir = os.path.realpath(skill_dir)
        self._on_changed = on_changed
        self._stop_event = asyncio.Event()

    def stop(self) -> None:
        self._stop_event.set()

    async def run(self) -> None:
        try:
            import watchfiles  # type: ignore[import-untyped]
            await self._run_with_watchfiles(watchfiles)
        except ImportError:
            await self._run_polling()

    # ── watchfiles backend ────────────────────────────────────────────────────

    async def _run_with_watchfiles(self, watchfiles: Any) -> None:
        async for changes in watchfiles.awatch(
            self._dir,
            stop_event=self._stop_event,
        ):
            relevant = any(
                os.path.splitext(path)[1] in _WATCHED_EXTS
                for _change, path in changes
            )
            if relevant:
                await self._fire()

    # ── polling backend ───────────────────────────────────────────────────────

    async def _run_polling(self) -> None:
        last_snapshot = self._snapshot()
        while not self._stop_event.is_set():
            try:
                await asyncio.wait_for(self._stop_event.wait(), timeout=1.0)
            except asyncio.TimeoutError:
                pass
            current = self._snapshot()
            if current != last_snapshot:
                last_snapshot = current
                await self._fire()

    def _snapshot(self) -> dict[str, float]:
        """mtime map for all skill files in the directory."""
        result: dict[str, float] = {}
        try:
            for entry in os.scandir(self._dir):
                if os.path.splitext(entry.name)[1] in _WATCHED_EXTS:
                    try:
                        result[entry.name] = entry.stat().st_mtime
                    except OSError:
                        pass
        except OSError:
            pass
        return result

    # ── debounced fire ────────────────────────────────────────────────────────

    _debounce_task: asyncio.Task[None] | None = None

    async def _fire(self) -> None:
        if self._debounce_task and not self._debounce_task.done():
            self._debounce_task.cancel()
        self._debounce_task = asyncio.create_task(self._debounced_call())

    async def _debounced_call(self) -> None:
        await asyncio.sleep(0.2)
        result = self._on_changed(self._dir)
        if asyncio.iscoroutine(result):
            await result


@asynccontextmanager
async def watch_skill_dir(
    skill_dir: str,
    on_changed: OnChangedCallback,
) -> AsyncGenerator[SkillWatcher, None]:
    """Async context manager that starts the watcher and stops it on exit."""
    watcher = SkillWatcher(skill_dir, on_changed)
    task = asyncio.create_task(watcher.run())
    try:
        yield watcher
    finally:
        watcher.stop()
        task.cancel()
        try:
            await task
        except (asyncio.CancelledError, Exception):
            pass
