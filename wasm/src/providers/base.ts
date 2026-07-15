import type { ContentPart, Message, RenderedContext } from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"

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

function parseToolArguments(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

/** Map an audio MIME type to OpenAI's `input_audio.format` (accepts "mp3" | "wav").
 *  `audio/mpeg` must become "mp3", not the raw "mpeg" subtype. */
function openaiAudioFormat(mediaType: string | undefined): string {
  const sub = (mediaType ?? "audio/wav").split("/")[1]?.toLowerCase() ?? "wav"
  if (sub === "mpeg" || sub === "mp3") return "mp3"
  if (sub === "wav" || sub === "wave" || sub === "x-wav") return "wav"
  return sub
}

/** Multimodal: OpenAI content blocks from contentParts (text + image). */
function openAIPartsContent(parts: ContentPart[]): Array<Record<string, unknown>> {
  return parts.map(p => {
    if (p.type === "image") {
      const url = p.data ? `data:${p.mediaType ?? "image/png"};base64,${p.data}` : (p.url ?? "")
      return { type: "image_url", image_url: { url, ...(p.detail ? { detail: p.detail } : {}) } }
    }
    if (p.type === "audio") {
      // OpenAI audio has no URL form — a part without base64 data cannot be sent.
      if (!p.data) throw new UnsupportedModalityError("audio (no data)", "openai")
      return {
        type: "input_audio",
        input_audio: { data: p.data, format: openaiAudioFormat(p.mediaType) },
      }
    }
    return { type: "text", text: p.text ?? p.output ?? "" }
  })
}

/** Multimodal: Anthropic content blocks from contentParts (text + image). */
function anthropicPartsContent(parts: ContentPart[]): Array<Record<string, unknown>> {
  return parts.map(p => {
    if (p.type === "image") {
      const source = p.data
        ? { type: "base64", media_type: p.mediaType ?? "image/png", data: p.data }
        : { type: "url", url: p.url ?? "" }
      return { type: "image", source }
    }
    if (p.type === "audio") {
      throw new UnsupportedModalityError("audio", "anthropic")
    }
    return { type: "text", text: p.text ?? p.output ?? "" }
  })
}

/** History turns with the volatile State turn appended as the latest turn
 *  (OpenAI), keeping the history a stable cacheable prefix. Anthropic appends it
 *  after the cache breakpoint. Absent on un-rebuilt bindings — then the state is
 *  already inside `turns`. */
export function turnsWithStateAppended(context: RenderedContext): Message[] {
  return context.stateTurn ? [...context.turns, context.stateTurn] : context.turns
}

/** Build OpenAI-compatible chat messages from a RenderedContext. */
export function toOpenAIMessages(context: RenderedContext): Array<Record<string, unknown>> {
  const messages: Array<Record<string, unknown>> = []
  if (context.systemText) {
    messages.push({ role: "system", content: context.systemText })
  }
  for (const msg of turnsWithStateAppended(context)) {
    if (msg.role === "tool") {
      messages.push({ role: "tool", content: msg.content })
      continue
    }
    const next: Record<string, unknown> = {
      role: msg.role,
      content: msg.contentParts?.length ? openAIPartsContent(msg.contentParts) : msg.content,
    }
    if (msg.role === "assistant" && msg.toolCalls?.length) {
      next.tool_calls = msg.toolCalls.map(tc => ({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: tc.arguments },
      }))
    }
    messages.push(next)
  }
  return messages
}

export function toAnthropicMessages(
  context: RenderedContext,
  nativeReplay?: (message: Message) => Array<Record<string, unknown>> | undefined,
): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of context.turns) {
    if (msg.role === "tool") {
      const parts = (msg.contentParts ?? [])
        .filter(p => p.type === "tool_result")
        .map(p => ({ type: "tool_result", tool_use_id: p.callId, content: p.output, is_error: p.isError ?? false }))
      if (parts.length) {
        result.push({ role: "user", content: parts })
      } else {
        // Fallback for messages without structured contentParts
        result.push({ role: "user", content: [{ type: "tool_result", tool_use_id: "", content: msg.content, is_error: false }] })
      }
      continue
    }

    if (msg.role === "assistant" && msg.toolCalls?.length) {
      const replay = nativeReplay?.(msg)
      if (replay) {
        result.push({ role: "assistant", content: replay })
        continue
      }
      const blocks: Array<Record<string, unknown>> = []
      if (msg.content) blocks.push({ type: "text", text: msg.content })
      blocks.push(...msg.toolCalls.map(tc => ({
        type: "tool_use",
        id: tc.id,
        name: tc.name,
        input: parseToolArguments(tc.arguments),
      })))
      result.push({ role: "assistant", content: blocks })
      continue
    }

    result.push({
      role: msg.role,
      content: msg.contentParts?.length ? anthropicPartsContent(msg.contentParts) : msg.content,
    })
  }

  return result
}

/** Collect a non-streaming assistant Message from stream events. */
export async function collectStreamMessage(
  stream: AsyncIterable<{ type: string; delta?: string; id?: string; name?: string; arguments?: Record<string, unknown> }>,
): Promise<Message> {
  let content = ""
  const toolCalls: Message["toolCalls"] = []
  let outputTokens: number | undefined
  for await (const evt of stream) {
    if (evt.type === "text_delta" && evt.delta) content += evt.delta
    else if (evt.type === "tool_call" && evt.id && evt.name) {
      toolCalls.push({ id: evt.id, name: evt.name, arguments: JSON.stringify(evt.arguments ?? {}) })
    } else if (evt.type === "usage") {
      outputTokens = (evt as { outputTokens?: number; totalTokens?: number }).outputTokens ?? (evt as { totalTokens?: number }).totalTokens
    }
  }
  return { role: "assistant", content, ...(outputTokens ? { tokenCount: outputTokens } : {}), ...(toolCalls.length ? { toolCalls } : {}) }
}

export { assistantReplayKey }
