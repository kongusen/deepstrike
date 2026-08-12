"""Protocol-level content disposition for canonical provider inputs."""
from __future__ import annotations

from typing import Literal

InputModality = Literal["text", "image", "audio", "video", "file"]
ContentPlacement = Literal["message", "tool_result"]
ContentDisposition = Literal["native", "bridge", "unsupported"]


def content_disposition_for(
  protocol: str,
  modality: InputModality,
  placement: ContentPlacement,
) -> ContentDisposition:
  if modality in {"text", "image"}:
    if protocol == "ollama-chat" and modality == "image" and placement == "tool_result":
      return "bridge"
    return "native"

  if protocol == "openai-responses" and modality == "file" and placement == "message":
    return "native"
  if protocol == "openai-responses" and modality == "file" and placement == "tool_result":
    return "bridge"

  if protocol == "openai-chat" and modality in {"audio", "file", "video"}:
    return "bridge"
  if protocol == "gemini" and modality in {"audio", "file", "video"}:
    return "bridge"
  return "unsupported"


class ContentPolicyError(ValueError):
  def __init__(self, protocol: str, modality: InputModality, placement: ContentPlacement) -> None:
    self.protocol = protocol
    self.modality = modality
    self.placement = placement
    super().__init__(
      f"Unsupported content policy: {modality} {placement} is not supported by {protocol}"
    )


def require_content_disposition(
  protocol: str,
  modality: InputModality,
  placement: ContentPlacement,
) -> ContentDisposition:
  disposition = content_disposition_for(protocol, modality, placement)
  if disposition == "unsupported":
    raise ContentPolicyError(protocol, modality, placement)
  return disposition
