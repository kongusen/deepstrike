"""spc_012: SDK-level structured multimodal content, pure-Python layer.

Mirrors the Node TS-only design (spc_012-N-01): the pyo3 `ContentPartObj` binding
deliberately gained no new field (spc_012-R-07 premise correction — that binding only
serves the kernel-JSON conversion path, which is text-only by design). Structured tool
output therefore lives on this pure-Python shim, duck-typed against `ContentPartObj`
(`type`/`call_id`/`output`/`is_error`), carried from an execution plane's
`ToolResultEvent.content_parts` through the runner's call_id side channel
(`RuntimeRunner._with_structured_tool_outputs`) to the provider serialization layer
(`providers/base.py`).

ContentBlocks are plain dicts (no class hierarchy — they only ever cross into provider
serializers, which pattern-match on `block["type"]`):

  {"type": "text", "text": str}
  {"type": "image", "source": {"kind": "base64", "data": str} | {"kind": "url", "url": str}, "media_type": str?}
  {"type": "audio", "source": {...}, "media_type": str?}
"""
from __future__ import annotations

from dataclasses import dataclass


@dataclass
class StructuredToolResultPart:
  """A tool_result content part whose `content_parts` hold the structured blocks.
  `output` remains the text projection. The current model cannot enforce that it agrees
  with `content_parts`; SPC-013 A-02 replaces this dual-source representation."""
  type: str = "tool_result"
  call_id: str = ""
  output: str = ""
  is_error: bool = False
  content_parts: list[dict] | None = None


@dataclass
class RenderedMessage:
  """Duck-typed stand-in for the pyo3 `_kernel.Message` on the provider-facing rendered
  path. The pyo3 Message constructor enforces `ContentPartObj` parts, so a structured
  tool_result part (a pure-Python `StructuredToolResultPart`) cannot be embedded in one;
  the runner's side-channel re-attachment (`RuntimeRunner._with_structured_tool_outputs`)
  therefore swaps affected messages for this plain carrier. Provider serializers
  (`providers/base.py` `to_anthropic_messages`/`to_openai_message_params`, gemini/ollama
  equivalents) are all attribute-based (`getattr`/`msg.role`/...), never
  `isinstance(msg, Message)` — duck-typing is sufficient and no provider is
  isinstance-strict on turns."""
  role: str = "user"
  content: str = ""
  token_count: int | None = None
  tool_calls: list | None = None
  content_parts: list | None = None
