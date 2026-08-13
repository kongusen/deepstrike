export type ContentPlacement = "message" | "tool_result"
export type ContentDisposition = "native" | "bridge" | "unsupported"
export type InputModality = "text" | "image" | "audio" | "video" | "file"

/**
 * Provider serialization policy for canonical content. A bridge preserves the deterministic
 * visible text projection; it never permits a serializer to drop the block or replace it with
 * empty content.
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
  if (protocol === "openai-chat" && (modality === "audio" || modality === "file" || modality === "video")) return "bridge"
  if (protocol === "gemini" && (modality === "audio" || modality === "file" || modality === "video")) return "bridge"
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
