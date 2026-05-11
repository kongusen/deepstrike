from pathlib import Path
from deepstrike.tools.registry import tool


@tool
def read_file(path: str) -> str:
    """Read the contents of a file at the given path."""
    return Path(path).expanduser().read_text(encoding="utf-8")
