import json
from pathlib import Path
from typing import Protocol
from deepstrike._kernel import ProviderMessage, ToolCall, ContentPartObj
from deepstrike.types.content import media_source

class ArchiveStore(Protocol):
    async def write(self, session_id: str, seq: int, messages: list[ProviderMessage]) -> str: ...
    async def read(self, archive_ref: str) -> list[ProviderMessage]: ...

class NullArchiveStore:
    async def write(self, session_id: str, seq: int, messages: list[ProviderMessage]) -> str:
        return ""

    async def read(self, archive_ref: str) -> list[ProviderMessage]:
        raise FileNotFoundError("NullArchiveStore does not store archives")

class FileArchiveStore:
    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)

    async def write(self, session_id: str, seq: int, messages: list[ProviderMessage]) -> str:
        dir_path = self.root / session_id
        dir_path.mkdir(parents=True, exist_ok=True)
        file_path = dir_path / f"{seq}.jsonl"
        
        lines = []
        for msg in messages:
            # Convert ProviderMessage object to dict for serialization
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
                    source = media_source(p) if getattr(p, "type", None) in {"image", "audio"} else None
                    parts_json.append({
                        "type": p.type,
                        "text": getattr(p, "text", None),
                        "source": source,
                        "media_type": getattr(p, "media_type", None),
                        "detail": getattr(p, "detail", None),
                        "call_id": getattr(p, "call_id", None),
                        "output": getattr(p, "output", None),
                        "is_error": getattr(p, "is_error", None),
                        "file_id": getattr(p, "file_id", None),
                        "provider_id": getattr(p, "provider_id", None),
                        "endpoint_id": getattr(p, "endpoint_id", None),
                    })
            lines.append(json.dumps({
                "role": msg.role,
                "content": msg.content,
                "tool_calls": tc_list,
                "content_parts": parts_json,
            }, ensure_ascii=False))
            
        with file_path.open("w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        return str(file_path)

    async def read(self, archive_ref: str) -> list[ProviderMessage]:
        file_path = Path(archive_ref)
        if not file_path.exists():
            raise FileNotFoundError(f"Archive not found: {archive_ref}")
        messages = []
        with file_path.open("r", encoding="utf-8") as f:
            for line in f:
                if not line.strip():
                    continue
                data = json.loads(line)
                tc_list = []
                for tc in data.get("tool_calls", []):
                    tc_list.append(ToolCall(
                        id=tc["id"],
                        name=tc["name"],
                        arguments=tc["arguments"],
                    ))
                
                parts_list = None
                content_parts = data.get("content_parts")
                if content_parts is not None:
                    parts_list = []
                    for p in content_parts:
                        source = p.get("source") or {}
                        part = ContentPartObj(
                            type=p["type"],
                            text=p.get("text"),
                            url=source.get("url") if source.get("kind") == "url" else None,
                            source_kind=source.get("kind"),
                            source_data=source.get("data") or source.get("handle"),
                            media_type=p.get("media_type"),
                            detail=p.get("detail"),
                            call_id=p.get("call_id"),
                            output=p.get("output"),
                            is_error=p.get("is_error") or False,
                            file_id=p.get("file_id"),
                            provider_id=p.get("provider_id"),
                            endpoint_id=p.get("endpoint_id"),
                        )
                        parts_list.append(part)
                
                messages.append(ProviderMessage(
                    role=data["role"],
                    content=data["content"],
                    tool_calls=tc_list,
                    content_parts=parts_list,
                ))
        return messages
