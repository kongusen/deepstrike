import type { InputModality } from "./protocol-capabilities.js"

export type ContentPlacement = "message" | "tool_result"
export type ContentDisposition = "native" | "bridge" | "unsupported"

/**
 * Provider serialization policy for canonical content. `bridge` means the protocol has no
 * structured equivalent for that placement, but serializers may send the visible text projection.
 * It never permits dropping a block or replacing it with an empty string.
 */
export function contentDispositionFor(
  protocol: string,
  modality: InputModality,
  placement: ContentPlacement,
): ContentDisposition {
  if (modality === "text" || modality === "image") {
    if (protocol === "ollama-chat" && modality === "image" && placement === "tool_result") return "bridge"
    return "native"
  }

  if (protocol === "openai-responses" && modality === "file" && placement === "message") return "native"
  if (protocol === "openai-responses" && modality === "file" && placement === "tool_result") return "bridge"

  if (protocol === "openai-chat" && ["audio", "file", "video"].includes(modality)) return "bridge"
  if (protocol === "gemini" && ["audio", "file", "video"].includes(modality)) return "bridge"

  return "unsupported"
}

export class ContentPolicyError extends Error {
  constructor(
    readonly protocol: string,
    readonly modality: InputModality,
    readonly placement: ContentPlacement,
  ) {
    super(`Unsupported content policy: ${modality} ${placement} is not supported by ${protocol}`)
    this.name = "ContentPolicyError"
  }
}

export function requireContentDisposition(
  protocol: string,
  modality: InputModality,
  placement: ContentPlacement,
): ContentDisposition {
  const disposition = contentDispositionFor(protocol, modality, placement)
  if (disposition === "unsupported") throw new ContentPolicyError(protocol, modality, placement)
  return disposition
}
