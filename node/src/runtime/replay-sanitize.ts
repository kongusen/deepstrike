/** UTF-8-safe text helpers for session replay (defense in depth). */

/** Soft cap — only snip very large llm_completed bodies before preload. */
export const REPLAY_CONTENT_MAX_BYTES = 32_768

export function truncateBytesAtCharBoundary(text: string, maxBytes: number): string {
  const data = Buffer.from(text, "utf8")
  if (data.length <= maxBytes) return text
  let end = maxBytes
  while (end > 0) {
    const slice = data.subarray(0, end).toString("utf8")
    if (Buffer.byteLength(slice, "utf8") <= end) return slice
    end -= 1
  }
  return ""
}

export function sanitizeReplayText(
  text: string,
  maxBytes: number = REPLAY_CONTENT_MAX_BYTES,
): string {
  if (!text) return text
  const safe = Buffer.from(text, "utf8").toString("utf8")
  if (Buffer.byteLength(safe, "utf8") <= maxBytes) return safe
  const prefix = truncateBytesAtCharBoundary(safe, maxBytes)
  return `${prefix}… [replay truncated]`
}
