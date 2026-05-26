/** UTF-8-safe text helpers for session replay (defense in depth). */

export const REPLAY_CONTENT_MAX_BYTES = 32_768

export function truncateBytesAtCharBoundary(text: string, maxBytes: number): string {
  const data = new TextEncoder().encode(text)
  if (data.length <= maxBytes) return text
  let end = maxBytes
  while (end > 0) {
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(data.subarray(0, end))
    } catch {
      end -= 1
    }
  }
  return ""
}

export function sanitizeReplayText(
  text: string,
  maxBytes: number = REPLAY_CONTENT_MAX_BYTES,
): string {
  if (!text) return text
  const bytes = new TextEncoder().encode(text)
  if (bytes.length <= maxBytes) return text
  const prefix = truncateBytesAtCharBoundary(text, maxBytes)
  return `${prefix}… [replay truncated]`
}
