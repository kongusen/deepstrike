import type { ContentBlock, Message, ContentPart, RenderedContext } from "../types.js"

export class CircuitBreaker {
  private failures = 0
  private openedAt: number | null = null

  constructor(
    private readonly openAfter: number = 5,
    private readonly resetAfter: number = 60_000,
  ) {}

  isOpen(): boolean {
    if (this.openedAt === null) return false
    if (Date.now() - this.openedAt >= this.resetAfter) {
      this.openedAt = null
      return false
    }
    return true
  }

  recordSuccess(): void {
    this.failures = 0
    this.openedAt = null
  }

  recordFailure(): void {
    this.failures++
    if (this.failures >= this.openAfter) this.openedAt = Date.now()
  }
}


/**
 * Internal control flags that steer DeepStrike's own serialization/validation
 * and must never be forwarded to any provider's wire request, regardless of the
 * per-call omit list.
 */
export const INTERNAL_EXTENSION_KEYS: readonly string[] = [
  "__deepstrikeThinkingEnabled",
  "degradeMissingReasoningReplay",
]

export function omitExtensionKeys(
  extensions: Record<string, unknown> | undefined,
  keys: readonly string[],
): Record<string, unknown> {
  if (!extensions) return {}
  const blocked = new Set([...keys, ...INTERNAL_EXTENSION_KEYS])
  return Object.fromEntries(Object.entries(extensions).filter(([key]) => !blocked.has(key)))
}

/**
 * Cached-prompt-token count from an OpenAI-compatible usage object. Covers the
 * standard `prompt_tokens_details.cached_tokens` (OpenAI, Qwen, MiniMax, GLM,
 * Kimi) and DeepSeek's `prompt_cache_hit_tokens`. These caches bill reads only,
 * so there is no separate cache-creation count. The figure is a subset of
 * `prompt_tokens` (the full prompt), surfaced for cost visibility — it must not
 * be subtracted from the input count the kernel uses for context accounting.
 */
export function openAICachedPromptTokens(usage: unknown): number {
  if (!usage || typeof usage !== "object") return 0
  const u = usage as Record<string, unknown>
  const details = u.prompt_tokens_details as Record<string, unknown> | undefined
  const standard = typeof details?.cached_tokens === "number" ? details.cached_tokens : 0
  const deepseek = typeof u.prompt_cache_hit_tokens === "number" ? u.prompt_cache_hit_tokens : 0
  return Math.max(standard, deepseek)
}

/**
 * Prompt-cache hit rate for one usage record: the fraction of the full prompt
 * served from cache this request (`cacheReadInputTokens / inputTokens`, clamped to
 * [0,1]). Returns 0 when the prompt size is unknown. This is the headline metric
 * for the prefix-cache work (P0-A) — across a long, append-only session it should
 * climb and stay high; a sustained drop means the cacheable prefix is drifting.
 */
export function cacheHitRate(usage: { inputTokens?: number; cacheReadInputTokens?: number }): number {
  const input = usage.inputTokens ?? 0
  if (input <= 0) return 0
  const read = usage.cacheReadInputTokens ?? 0
  return Math.min(1, Math.max(0, read / input))
}

/**
 * Deterministic short key for OpenAI's `prompt_cache_key` — groups requests that
 * share a cacheable prefix (same system prompt + tool set) onto the same cache
 * routing, improving automatic prefix-cache hit rates without any caller input.
 * FNV-1a over the parts; stable across processes, no crypto dependency.
 */
export function stablePromptCacheKey(parts: string[]): string {
  let hash = 0x811c9dc5
  const joined = parts.join("")
  for (let i = 0; i < joined.length; i++) {
    hash ^= joined.charCodeAt(i)
    hash = Math.imul(hash, 0x01000193)
  }
  return `ds-${(hash >>> 0).toString(16).padStart(8, "0")}`
}

export function normalizeToolCall(id: string, name: string, args: unknown): { id: string; name: string; arguments: string } | null {
  const n = String(name ?? "").trim()
  if (!n) return null
  let parsed: Record<string, unknown> = {}
  if (typeof args === "string") {
    try { parsed = JSON.parse(args || "{}") } catch { parsed = {} }
  } else if (args && typeof args === "object") {
    parsed = args as Record<string, unknown>
  }
  return { id: String(id ?? ""), name: n, arguments: JSON.stringify(parsed) }
}

function parseToolArguments(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

// ─── Anthropic message conversion ────────────────────────────────────────────

export class UnsupportedModalityError extends Error {
  readonly modality: string
  readonly provider: string
  constructor(modality: string, provider: string) {
    super(`UnsupportedModality: ${modality} is not supported by ${provider}`)
    this.name = "UnsupportedModalityError"
    this.modality = modality
    this.provider = provider
  }
}

export function toAnthropicContent(msg: Message): string | Array<Record<string, unknown>> {
  if (!msg.contentParts?.length) return msg.content
  return msg.contentParts.map(p => {
    if (p.type === "text") return { type: "text", text: p.text }
    if (p.type === "image") {
      if (p.data) {
        return { type: "image", source: { type: "base64", media_type: p.mediaType ?? "image/png", data: p.data } }
      }
      return { type: "image", source: { type: "url", url: p.url } }
    }
    if (p.type === "audio") {
      throw new UnsupportedModalityError("audio", "anthropic")
    }
    if (p.type === "tool_result") {
      return { type: "tool_result", tool_use_id: p.callId, content: toolResultAnthropicContent(p), is_error: p.isError }
    }
    return { type: "text", text: "" }
  })
}

/**
 * spc_012-N-03: a `ContentBlock` inside a structured tool result → Anthropic wire block.
 * Anthropic's `tool_result.content` natively accepts text and image blocks. Anything else
 * (audio/video/file, `fileId`/`object` sources with no Anthropic wire form) degrades to an
 * explicit `[modality]` placeholder visible to the model, never silently dropped (INV-012-01).
 */
function contentBlockToAnthropic(block: ContentBlock): Record<string, unknown> {
  if (block.type === "text") return { type: "text", text: block.text }
  if (block.type === "image") {
    const src = block.source
    if (src.kind === "base64") {
      return { type: "image", source: { type: "base64", media_type: block.mediaType ?? "image/png", data: src.data } }
    }
    if (src.kind === "url") {
      return { type: "image", source: { type: "url", url: src.url } }
    }
    return { type: "text", text: "[image]" }
  }
  if (block.type === "tool_result") {
    // INV-012-03 forbids nesting; flatten defensively rather than recurse.
    return { type: "text", text: "[tool_result]" }
  }
  return { type: "text", text: `[${block.type}]` }
}

/**
 * spc_012-N-03: structured `contentParts` win when present; otherwise the legacy text
 * projection (`output`) is the content, byte-identical to the pre-spc_012 behavior.
 */
function toolResultAnthropicContent(p: Extract<ContentPart, { type: "tool_result" }>): string | Array<Record<string, unknown>> {
  if (p.contentParts?.length) return p.contentParts.map(contentBlockToAnthropic)
  return p.output
}

/**
 * History turns with the volatile State turn appended as the latest turn, for
 * providers that render it inline (OpenAI-family, Gemini, Ollama). Appending
 * (rather than prepending) keeps the history a byte-stable prefix so these
 * providers' automatic prefix caches (OpenAI / Gemini implicit / Ollama KV) hit
 * across turns — the volatile state is the uncached tail. Anthropic does the
 * equivalent explicitly (append after the cache breakpoint — see
 * AnthropicProvider.buildMessages). When `stateTurn` is absent (un-rebuilt
 * binding) the State turn is still inside `turns`, so this returns `turns` as-is.
 */
export function turnsWithStateAppended(context: RenderedContext): Message[] {
  return context.stateTurn ? [...context.turns, context.stateTurn] : context.turns
}

/** Convert RenderedContext.turns to Anthropic messages array.
 *  `turns` contains only user / assistant / tool roles — no system filtering needed. */
export function toAnthropicMessages(
  turns: Message[],
  nativeReplay?: (message: Message) => Array<Record<string, unknown>> | undefined,
): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of turns) {
    if (msg.role === "tool") {
      const parts = (msg.contentParts ?? [])
        .filter((p): p is Extract<ContentPart, { type: "tool_result" }> => p.type === "tool_result")
        .map(p => ({ type: "tool_result", tool_use_id: p.callId, content: toolResultAnthropicContent(p), is_error: p.isError }))
      if (parts.length) result.push({ role: "user", content: parts })
      continue
    }

    if (msg.role === "assistant" && msg.toolCalls?.length) {
      const replay = nativeReplay?.(msg)
      if (replay) {
        result.push({ role: "assistant", content: ensureAssistantToolText(replay) })
        continue
      }
      const blocks: Array<Record<string, unknown>> = []
      if (msg.content) blocks.push({ type: "text", text: msg.content })
      else blocks.push({ type: "text", text: "Tool call requested." })
      blocks.push(...msg.toolCalls.map(tc => ({
        type: "tool_use",
        id: tc.id,
        name: tc.name,
        input: parseToolArguments(tc.arguments),
      })))
      result.push({ role: "assistant", content: blocks })
      continue
    }

    result.push({ role: msg.role, content: toAnthropicContent(msg) })
  }

  return result
}

function ensureAssistantToolText(blocks: Array<Record<string, unknown>>): Array<Record<string, unknown>> {
  if (!blocks.some(b => b.type === "tool_use")) return blocks
  if (blocks.some(b => b.type === "text" && String(b.text ?? "").trim())) return blocks
  if (blocks.some(b => b.type === "thinking")) return blocks
  return [{ type: "text", text: "Tool call requested." }, ...blocks]
}

// ─── OpenAI-compatible message conversion ────────────────────────────────────

/** Map an audio MIME type to OpenAI's `input_audio.format` (accepts "mp3" | "wav").
 *  `audio/mpeg` must become "mp3", not the raw "mpeg" subtype. */
export function openaiAudioFormat(mediaType: string | undefined): string {
  const sub = (mediaType ?? "audio/wav").split("/")[1]?.toLowerCase() ?? "wav"
  if (sub === "mpeg" || sub === "mp3") return "mp3"
  if (sub === "wav" || sub === "wave" || sub === "x-wav") return "wav"
  return sub
}

export function toOpenAIContent(msg: Message): string | Array<Record<string, unknown>> {
  if (!msg.contentParts?.length) return msg.content
  return msg.contentParts.map(p => {
    if (p.type === "text") return { type: "text", text: p.text }
    if (p.type === "image") {
      const url = p.data ? `data:${p.mediaType ?? "image/png"};base64,${p.data}` : p.url!
      return { type: "image_url", image_url: { url, ...(p.detail ? { detail: p.detail } : {}) } }
    }
    if (p.type === "audio") {
      return { type: "input_audio", input_audio: { data: p.data, format: openaiAudioFormat(p.mediaType) } }
    }
    if (p.type === "tool_result") {
      return { type: "text", text: p.output }
    }
    return { type: "text", text: "" }
  })
}

/** Build the full OpenAI messages array from a RenderedContext.
 *  Prepends systemText as the first system message, then converts turns. */
export function toOpenAIMessageParams(context: RenderedContext): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  if (context.systemText) {
    result.push({ role: "system", content: context.systemText })
  }

  // The volatile State turn is appended as the latest turn so the history stays a
  // stable prefix that OpenAI's automatic cache can hit. Absent on un-rebuilt
  // bindings, where the state is already inside `turns`.
  for (const msg of turnsWithStateAppended(context)) {
    if (msg.role === "tool") {
      // spc_012-N-04: OpenAI **chat completions** tool-role messages accept text only — no
      // structured tool_result content exists on this wire. Explicit text-only degradation:
      // `output` is the constructor's text projection, which carries a visible `[modality]`
      // placeholder for any structured block (INV-012-01 — degraded, not silently dropped).
      // The Responses API (openai-responses.ts) does have native structured support and uses it.
      const parts = (msg.contentParts ?? [])
        .filter((p): p is Extract<ContentPart, { type: "tool_result" }> => p.type === "tool_result")
      for (const p of parts) {
        result.push({ role: "tool", tool_call_id: p.callId, content: p.output })
      }
      continue
    }

    const next: Record<string, unknown> = {
      role: msg.role,
      content: toOpenAIContent(msg),
    }
    if (msg.role === "assistant" && msg.toolCalls?.length) {
      next.tool_calls = msg.toolCalls.map(tc => ({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: tc.arguments },
      }))
    }
    result.push(next)
  }

  return result
}

export class ThinkingTagStreamExtractor {
  private buffer = ""
  private inThinking = false

  feed(chunk: string): Array<{ type: "text" | "thinking"; content: string }> {
    this.buffer += chunk
    const events: Array<{ type: "text" | "thinking"; content: string }> = []

    while (true) {
      if (!this.inThinking) {
        const thinkIndex = this.buffer.indexOf("<think>")
        if (thinkIndex !== -1) {
          const textBefore = this.buffer.substring(0, thinkIndex)
          if (textBefore) {
            events.push({ type: "text", content: textBefore })
          }
          this.inThinking = true
          this.buffer = this.buffer.substring(thinkIndex + 7)
          continue
        }

        const possibleTagStart = this.buffer.lastIndexOf("<")
        if (possibleTagStart !== -1 && "<think>".startsWith(this.buffer.substring(possibleTagStart))) {
          const toEmit = this.buffer.substring(0, possibleTagStart)
          if (toEmit) {
            events.push({ type: "text", content: toEmit })
          }
          this.buffer = this.buffer.substring(possibleTagStart)
          break
        } else {
          if (this.buffer) {
            events.push({ type: "text", content: this.buffer })
            this.buffer = ""
          }
          break
        }
      } else {
        const endThinkIndex = this.buffer.indexOf("</think>")
        if (endThinkIndex !== -1) {
          const thinkingContent = this.buffer.substring(0, endThinkIndex)
          if (thinkingContent) {
            events.push({ type: "thinking", content: thinkingContent })
          }
          this.inThinking = false
          this.buffer = this.buffer.substring(endThinkIndex + 8)
          continue
        }

        const possibleEndStart = this.buffer.lastIndexOf("<")
        if (possibleEndStart !== -1 && "</think>".startsWith(this.buffer.substring(possibleEndStart))) {
          const toEmit = this.buffer.substring(0, possibleEndStart)
          if (toEmit) {
            events.push({ type: "thinking", content: toEmit })
          }
          this.buffer = this.buffer.substring(possibleEndStart)
          break
        } else {
          if (this.buffer) {
            events.push({ type: "thinking", content: this.buffer })
            this.buffer = ""
          }
          break
        }
      }
    }
    return events
  }

  flush(): Array<{ type: "text" | "thinking"; content: string }> {
    const events: Array<{ type: "text" | "thinking"; content: string }> = []
    if (this.buffer) {
      events.push({ type: this.inThinking ? "thinking" : "text", content: this.buffer })
      this.buffer = ""
    }
    return events
  }
}
