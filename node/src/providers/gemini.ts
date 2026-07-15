import { GoogleGenerativeAI, type Content, type Part, type RequestOptions, type Tool } from "@google/generative-ai"
import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, normalizeToolCall, turnsWithStateAppended } from "./base.js"
import { endpointProfiles } from "./profiles.js"
import { UnsupportedModalityError } from "./base.js"

const GEMINI_BASE = (endpointProfiles as Record<string, { baseURL: string }>)["gemini.google"].baseURL

const GEMINI_POLICIES: Record<string, RuntimePolicy> = {
  "gemini-3-pro-preview":   { maxTurns: 50 },
  "gemini-3-flash-preview": { maxTurns: 25 },
  "gemini-3.5-flash":       { maxTurns: 30 },
  "gemini-2.5-pro":        { maxTurns: 35 },
  "gemini-2.5-flash":      { maxTurns: 20 },
  "gemini-2.0-flash":      { maxTurns: 15 },
  "gemini-2.0-flash-lite": { maxTurns: 10 },
  "gemini-1.5-pro":        { maxTurns: 30 },
  "gemini-1.5-flash":      { maxTurns: 15 },
}

export function buildContents(turns: Message[]): Content[] {
  const contents: Content[] = []
  for (const msg of turns) {
    if (msg.role === "tool") {
      const parts: Part[] = (msg.contentParts ?? [])
        .filter(p => p.type === "tool_result")
        .map(p => {
          if (p.type !== "tool_result") return { text: "" }
          let toolName = p.callId
          for (let i = turns.length - 1; i >= 0; i--) {
            const turn = turns[i]
            if (turn.role === "assistant" && turn.toolCalls) {
              const matched = turn.toolCalls.find(tc => tc.id === p.callId)
              if (matched) {
                toolName = matched.name
                break
              }
            }
          }
          return {
            functionResponse: { name: toolName, response: { output: p.output } },
          }
        })
      if (parts.length) contents.push({ role: "user", parts })
      continue
    }
    const role = msg.role === "assistant" ? "model" : "user"
    const parts: Part[] = []
    if (msg.toolCalls?.length) {
      for (const tc of msg.toolCalls) {
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(tc.arguments) } catch { args = {} }
        parts.push({ functionCall: { name: tc.name, args } })
      }
    }
    // Multimodal: render contentParts (text + image) when present, else the plain
    // text body. Without this, image inputs to Gemini were silently dropped.
    if (msg.contentParts?.length) {
      for (const p of msg.contentParts) {
        if (p.type === "text") parts.push({ text: p.text })
        else if (p.type === "image") {
          if (p.data) parts.push({ inlineData: { mimeType: p.mediaType ?? "image/png", data: p.data } })
          else if (p.url) parts.push({ fileData: { mimeType: p.mediaType ?? "image/png", fileUri: p.url } } as Part)
        } else if (p.type === "audio") {
          if (!p.data) throw new UnsupportedModalityError("audio", "gemini")
          parts.push({ inlineData: { mimeType: p.mediaType ?? "audio/wav", data: p.data } })
        } else if (p.type === "tool_result") {
          // tool results are handled via functionResponse on tool role messages
        } else {
          throw new UnsupportedModalityError(String((p as { type?: string }).type ?? "unknown"), "gemini")
        }
      }
    } else if (msg.content) {
      parts.push({ text: msg.content })
    }
    if (parts.length) contents.push({ role, parts })
  }
  return contents
}

function buildTools(tools: ToolSchema[]): Tool[] {
  if (!tools.length) return []
  return [{
    functionDeclarations: tools.map(t => ({
      name: t.name,
      description: t.description,
      parameters: JSON.parse(t.parameters),
    })),
  }]
}

export class GeminiProvider implements LLMProvider {
  private genAI: GoogleGenerativeAI
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number
  private requestOptions: RequestOptions

  constructor(
    apiKey: string,
    private readonly model = "gemini-2.0-flash",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL: string = GEMINI_BASE,
  ) {
    this.genAI = withServerRuntimeGuard(() => new GoogleGenerativeAI(apiKey))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
    this.requestOptions = { baseUrl: baseURL }
  }

  runtimePolicy(): RuntimePolicy {
    return GEMINI_POLICIES[this.model] ?? {}
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const system = context.systemText || undefined
    const contents = buildContents(turnsWithStateAppended(context))
    const geminiTools = buildTools(tools)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const vc = this.vendorConfig(extensions)
        const allTools = [...geminiTools, ...((vc.tools as Tool[] | undefined) ?? [])]
        const m = this.genAI.getGenerativeModel({
          ...this.modelExtensions(extensions),
          model: this.model,
          ...(system ? { systemInstruction: system } : {}),
          ...(allTools.length ? { tools: allTools } : {}),
          ...(vc.generationConfig ? { generationConfig: vc.generationConfig } : {}),
        }, this.requestOptions)
        const resp = await m.generateContent({ contents })
        this.circuit.recordSuccess()
        const candidate = resp.response.candidates?.[0]
        let content = ""
        const toolCalls = []
        for (const part of candidate?.content.parts ?? []) {
          if (part.text) content += part.text
          else if (part.functionCall) {
            const tc = normalizeToolCall(part.functionCall.name, part.functionCall.name, part.functionCall.args)
            if (tc) toolCalls.push(tc)
          }
        }
        const usage = resp.response.usageMetadata
        return {
          role: "assistant",
          content,
          tokenCount: usage?.candidatesTokenCount ?? usage?.totalTokenCount,
          toolCalls,
        }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = context.systemText || undefined
    const contents = buildContents(turnsWithStateAppended(context))
    const geminiTools = buildTools(tools)

    const vc = this.vendorConfig(extensions)
    const allTools = [...geminiTools, ...((vc.tools as Tool[] | undefined) ?? [])]
    const m = this.genAI.getGenerativeModel({
      ...this.modelExtensions(extensions),
      model: this.model,
      ...(system ? { systemInstruction: system } : {}),
      ...(allTools.length ? { tools: allTools } : {}),
      ...(vc.generationConfig ? { generationConfig: vc.generationConfig } : {}),
    }, this.requestOptions)

    const result = await m.generateContentStream({ contents })
    const toolCalls: Array<{ id: string; name: string; args: Record<string, unknown> }> = []

    for await (const chunk of result.stream) {
      for (const part of chunk.candidates?.[0]?.content.parts ?? []) {
        if (part.text) yield { type: "text_delta", delta: part.text } as TextDelta
        else if (part.functionCall) {
          const { name, args } = part.functionCall
          toolCalls.push({ id: `call_${toolCalls.length + 1}`, name, args: args as Record<string, unknown> })
        }
      }
    }

    for (const tc of toolCalls) {
      yield { type: "tool_call", id: tc.id, name: tc.name, arguments: tc.args } as ToolCallEvent
    }

    const usage = (await result.response).usageMetadata
    if (usage?.totalTokenCount) {
      // Gemini implicit/explicit cache hits are reported as cachedContentTokenCount,
      // a subset of promptTokenCount (which stays the full prompt for accounting).
      const cachedTokens = (usage as { cachedContentTokenCount?: number }).cachedContentTokenCount ?? 0
      yield {
        type: "usage",
        totalTokens: usage.totalTokenCount,
        inputTokens: usage.promptTokenCount ?? 0,
        outputTokens: usage.candidatesTokenCount ?? 0,
        ...(cachedTokens > 0 ? { cacheReadInputTokens: cachedTokens } : {}),
      } as StreamEvent
    }
  }

  private modelExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    if (!extensions) return {}
    // Strip keys handled explicitly elsewhere (incl. the vendor server-tool / structured-output keys
    // consumed by `vendorConfig`) so they never leak raw into getGenerativeModel.
    // Strip keys handled explicitly: the SDK fields set below + the named vendor keys consumed by
    // `vendorConfig`. A caller-provided raw `generationConfig` still passes through (and is merged with
    // any structured-output config at the call site).
    const {
      model: _model, systemInstruction: _systemInstruction, tools: _tools,
      google_search: _gs, response_mime_type: _rmt, response_schema: _rs,
      ...rest
    } = extensions
    return rest
  }

  /**
   * Gemini vendor features from extensions, mapped to the Node SDK shape (mirrors the Python provider's
   * extension keys for a consistent cross-SDK API):
   *  - `google_search` (truthy → default, object → config): Google Search grounding server tool
   *    (gemini-2.0+), appended to tools[].
   *  - `response_mime_type` / `response_schema`: structured output → `generationConfig` (the API rejects
   *    pairing this with google_search).
   */
  vendorConfig(extensions?: Record<string, unknown>): { tools?: unknown[]; generationConfig?: Record<string, unknown> } {
    const ext = extensions ?? {}
    const tools: unknown[] = []
    if (ext.google_search) tools.push({ googleSearch: typeof ext.google_search === "object" ? ext.google_search : {} })
    // Seed from any caller-provided raw generationConfig, then layer the named structured-output keys.
    const gc: Record<string, unknown> = { ...(ext.generationConfig as Record<string, unknown> | undefined) }
    if (ext.response_mime_type != null) gc.responseMimeType = ext.response_mime_type
    if (ext.response_schema != null) gc.responseSchema = ext.response_schema
    return {
      ...(tools.length ? { tools } : {}),
      ...(Object.keys(gc).length ? { generationConfig: gc } : {}),
    }
  }
}
