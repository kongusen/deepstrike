from typing import Any


class WorkingMemory:
    def __init__(self):
        self.scratch_pad: dict[str, Any] = {}

    def set(self, key: str, value: Any):
        self.scratch_pad[key] = value

    def get(self, key: str, default: Any = None) -> Any:
        return self.scratch_pad.get(key, default)

    def clear(self):
        self.scratch_pad.clear()
