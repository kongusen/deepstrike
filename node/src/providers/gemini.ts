import { GoogleGenerativeAI, type Content, type Part, type Tool } from "@google/generative-ai"
import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, normalizeToolCall } from "./base.js"
import { endpointProfiles } from "./profiles.js"

const GEMINI_BASE = (endpointProfiles as Record<string, { baseURL: string }>)["gemini.google"].baseURL

function buildContents(turns: Message[]): Content[] {
  const contents: Content[] = []
  for (const msg of turns) {
    if (msg.role === "tool") {
      const parts: Part[] = (msg.contentParts ?? [])
        .filter(p => p.type === "tool_result")
        .map(p => p.type === "tool_result" ? ({
          functionResponse: { name: p.callId, response: { output: p.output } },
        }) : ({ text: "" }))
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
    if (msg.content) parts.push({ text: msg.content })
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
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const system = context.systemText || undefined
    const contents = buildContents(context.turns)
    const geminiTools = buildTools(tools)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const m = this.genAI.getGenerativeModel({
          ...this.modelExtensions(extensions),
          model: this.model,
          ...(system ? { systemInstruction: system } : {}),
          ...(geminiTools.length ? { tools: geminiTools } : {}),
        })
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
          tokenCount: usage?.totalTokenCount,
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
    const contents = buildContents(context.turns)
    const geminiTools = buildTools(tools)

    const m = this.genAI.getGenerativeModel({
      ...this.modelExtensions(extensions),
      model: this.model,
      ...(system ? { systemInstruction: system } : {}),
      ...(geminiTools.length ? { tools: geminiTools } : {}),
    })

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
    if (usage?.totalTokenCount) yield { type: "usage", totalTokens: usage.totalTokenCount } as StreamEvent
  }

  private modelExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    if (!extensions) return {}
    const { model: _model, systemInstruction: _systemInstruction, tools: _tools, ...rest } = extensions
    return rest
  }
}
