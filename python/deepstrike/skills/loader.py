from __future__ import annotations

import re
from pathlib import Path


_SAFE_SKILL_NAME = re.compile(r"^[A-Za-z0-9_-]+$")


def skill_path(skill_dir: str | Path, name: str) -> Path:
    if not _SAFE_SKILL_NAME.fullmatch(name):
        raise ValueError(f'invalid skill name "{name}": use only letters, digits, "-", "_"')
    return Path(skill_dir) / f"{name}.md"


def read_skill_file(skill_dir: str | Path, name: str) -> str | None:
    path = skill_path(skill_dir, name)
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    return re.sub(r"^---\n.*?\n---\n?", "", content, count=1, flags=re.DOTALL)
