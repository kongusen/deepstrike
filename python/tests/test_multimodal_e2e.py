"""Real-model MULTIMODAL e2e (image attachment) — mirrors node/tests/e2e/multimodal.test.ts.

Sends a synthesized two-color image through the framework end-to-end and checks the model
actually saw the pixels (top RED / bottom BLUE — unguessable from text alone), including across a
crash-and-resume rebuilt from the session log (the regression guard for the resume data-loss bug).

Run with:
    set -a; source .env; set +a; python -m pytest tests/test_multimodal_e2e.py -q
Automatically skipped when MINIMAX_API_KEY is absent.
"""
import base64
import os
import time
import zlib

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    MiniMaxAnthropicProvider,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
)

pytestmark = pytest.mark.skipif(
    not os.environ.get("MINIMAX_API_KEY"),
    reason="MINIMAX_API_KEY not set (real-model multimodal e2e)",
)


# ── minimal PNG encoder (no deps) ─────────────────────────────────────────────
def _chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return len(data).to_bytes(4, "big") + body + (zlib.crc32(body) & 0xFFFFFFFF).to_bytes(4, "big")


def _two_color_png(w: int, h: int, top: tuple, bottom: tuple) -> str:
    """Solid top-half / bottom-half RGB image as a base64 PNG."""
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # filter: none
        color = top if y < h // 2 else bottom
        for _ in range(w):
            raw.extend(color)
    ihdr = w.to_bytes(4, "big") + h.to_bytes(4, "big") + bytes([8, 2, 0, 0, 0])  # 8-bit RGB
    png = (
        bytes([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
        + _chunk(b"IHDR", ihdr)
        + _chunk(b"IDAT", zlib.compress(bytes(raw)))
        + _chunk(b"IEND", b"")
    )
    return base64.b64encode(png).decode()


def _provider() -> MiniMaxAnthropicProvider:
    return MiniMaxAnthropicProvider(os.environ["MINIMAX_API_KEY"], os.environ.get("MINIMAX_MODEL"))


_GOAL = (
    "Look at the attached image. It is split into two horizontal halves. Name the color of the "
    "TOP half and the color of the BOTTOM half. Answer in the form 'top: <color>, bottom: <color>'."
)


def _assert_saw_red_over_blue(text: str) -> None:
    assert "red" in text, f"model did not see the image; said: {text[:200]}"
    assert "blue" in text, f"model did not see the image; said: {text[:200]}"
    assert text.index("red") < text.index("blue"), f"colors out of order; said: {text[:200]}"


async def test_sees_image_via_run_attachments():
    b64 = _two_color_png(96, 96, (255, 0, 0), (0, 0, 255))  # red top, blue bottom
    runner = RuntimeRunner(RuntimeOptions(
        provider=_provider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=4000,
        max_turns=2,
    ))
    text = (await collect_text(runner.run(
        goal=_GOAL,
        session_id=f"mm-{int(time.time() * 1000)}",
        attachments=[{"type": "image", "data": b64, "media_type": "image/png"}],
    ))).lower()
    print(f"\n[py multimodal] model said: {text.strip()[:200]}\n")
    _assert_saw_red_over_blue(text)


async def test_image_survives_crash_and_resume():
    # Regression guard: run_started.attachments is persisted but was never read back on resume —
    # reconstruction rebuilt a TEXT-ONLY initial turn, so the image was lost after a crash. We
    # simulate the crash with a mid-run log (run_started + attachments, no run_terminal) and resume.
    b64 = _two_color_png(96, 96, (255, 0, 0), (0, 0, 255))
    log = InMemorySessionLog()
    sid = f"mm-resume-{int(time.time() * 1000)}"
    await log.append(sid, {
        "kind": "run_started",
        "run_id": "crashed-run",
        "goal": _GOAL,
        "criteria": [],
        "attachments": [{"type": "image", "data": b64, "media_type": "image/png"}],
    })
    runner = RuntimeRunner(RuntimeOptions(
        provider=_provider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=4000,
        max_turns=2,
    ))
    text = (await collect_text(runner.run(goal=_GOAL, session_id=sid))).lower()
    print(f"\n[py multimodal-resume] model said: {text.strip()[:200]}\n")
    _assert_saw_red_over_blue(text)
