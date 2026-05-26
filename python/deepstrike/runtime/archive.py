import json
from pathlib import Path
from typing import Protocol
from deepstrike._kernel import Message

class ArchiveStore(Protocol):
    async def write(self, session_id: str, seq: int, messages: list[Message]) -> str: ...

class NullArchiveStore:
    async def write(self, session_id: str, seq: int, messages: list[Message]) -> str:
        return ""

class FileArchiveStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)

    async def write(self, session_id: str, seq: int, messages: list[Message]) -> str:
        dir_path = self.root / session_id
        dir_path.mkdir(parents=True, exist_ok=True)
        file_path = dir_path / f"{seq}.jsonl"
        
        lines = []
        for msg in messages:
            # Convert Message object to dict for serialization
            tc_list = []
            for tc in getattr(msg, "tool_calls", []):
                tc_list.append({
                    "id": tc.id,
                    "name": tc.name,
                    "arguments": tc.arguments,
                })
            # content parts
            parts_json = None
            content_parts = getattr(msg, "content_parts", None)
            if content_parts is not None:
                parts_json = []
                for p in content_parts:
                    parts_json.append({
                        "type": p.type,
                        "text": getattr(p, "text", None),
                        "url": getattr(p, "url", None),
                        "data": getattr(p, "data", None),
                        "media_type": getattr(p, "media_type", None),
                        "detail": getattr(p, "detail", None),
                        "call_id": getattr(p, "call_id", None),
                        "output": getattr(p, "output", None),
                        "is_error": getattr(p, "is_error", None),
                    })
            lines.append(json.dumps({
                "role": msg.role,
                "content": msg.content,
                "tool_calls": tc_list,
                "token_count": msg.token_count,
                "content_parts": parts_json,
            }, ensure_ascii=False))
            
        with file_path.open("w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        return str(file_path)
