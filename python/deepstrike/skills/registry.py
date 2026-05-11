from __future__ import annotations
import pathlib
from typing import Any
from deepstrike._kernel import SkillMetadata


def _parse_frontmatter(text: str) -> dict[str, Any]:
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end == -1:
        return {}
    meta: dict[str, Any] = {}
    for line in text[3:end].splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            meta[k.strip()] = v.strip()
    return meta


class SkillRegistry:
    """Scans a directory of .md skill files and registers them with the kernel."""

    def __init__(self, skill_dir: str):
        self._dir = pathlib.Path(skill_dir)

    def scan(self) -> list[SkillMetadata]:
        skills = []
        for path in self._dir.glob("*.md"):
            text = path.read_text(encoding="utf-8")
            meta = _parse_frontmatter(text)
            name = meta.get("name") or path.stem
            skills.append(SkillMetadata(
                name=str(name),
                description=str(meta.get("description", "")),
                when_to_use=str(meta.get("when_to_use", "")) or None,
                effort=int(meta["effort"]) if "effort" in meta else None,
                estimated_tokens=int(meta.get("estimated_tokens", len(text) // 4)),
            ))
        return skills
