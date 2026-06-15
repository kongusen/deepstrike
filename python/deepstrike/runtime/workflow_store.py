"""M6: file-backed persistence for declarative ``WorkflowSpec``s — the SDK side of "save & share
workflows".

A spec is pure data, so a saved workflow is plain JSON that round-trips exactly. Check the files into
``~/.deepstrike/workflows/``, or ship them inside a skill as templates: put the JSON in the skill
folder and have the agent ``load()`` + (optionally) tweak the spec before ``run_workflow``.
"""

from __future__ import annotations

import json
import re
from dataclasses import asdict
from pathlib import Path

from deepstrike.types.agent import WorkflowNodeSpec, WorkflowSpec

_SAFE_NAME = re.compile(r"^[A-Za-z0-9_-]+$")


def _default_root() -> Path:
  return Path.home() / ".deepstrike" / "workflows"


def _safe_name(name: str) -> str:
  if not _SAFE_NAME.match(name):
    raise ValueError(f'invalid workflow name "{name}": use only letters, digits, "-", "_"')
  return name


class FileWorkflowStore:
  """File-backed ``WorkflowSpec`` store. Default root ``~/.deepstrike/workflows``; override via
  ``root_dir`` (e.g. a skill folder for distribution). One spec per ``<name>.json``."""

  def __init__(self, root_dir: str | Path | None = None) -> None:
    self._root = Path(root_dir) if root_dir is not None else _default_root()

  def save(self, name: str, spec: WorkflowSpec) -> str:
    """Persist ``spec`` under ``name``; returns the file path written."""
    path = self._root / f"{_safe_name(name)}.json"
    self._root.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(asdict(spec), indent=2), encoding="utf-8")
    return str(path)

  def load(self, name: str) -> WorkflowSpec:
    """Load the spec saved under ``name``. Raises ``FileNotFoundError`` if it does not exist."""
    path = self._root / f"{_safe_name(name)}.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    nodes = [WorkflowNodeSpec(**node) for node in data.get("nodes", [])]
    return WorkflowSpec(nodes=nodes)

  def list(self) -> list[str]:
    """The names of all saved workflows (sorted); ``[]`` when the store dir does not exist yet."""
    if not self._root.exists():
      return []
    return sorted(p.stem for p in self._root.glob("*.json"))
