import type {
  Content,
  GenerateContentRequest,
  GenerateContentResponse,
  ModelParams,
  Part,
  Tool,
} from "@google/generative-ai"

import type {
  Message,
  ProviderUsage,
  StreamEvent,
  TextDelta,
  ToolCall,
  ToolSchema,
  UsageEvent,
} from "../types.js"
import type {
  CanonicalAdapterInput,
  CanonicalMessage,
  CanonicalToolResult,
} from "./content-normalization.js"
import { projectToolOutputToText } from "./content-normalization.js"
import { normalizeToolCall } from "./base.js"
import type { ProtocolRuntimeCapabilities } from "./model-registry.js"
import {
  type AdapterDecodeInput,
  type AdapterOutput,
  type AdapterStreamInput,
  type CanonicalStopReason,
  type ProtocolAdapter,
  ProtocolResponseError,
} from "./protocol-adapter.js"

export const GEMINI_PROTOCOL_CAPABILITIES: ProtocolRuntimeCapabilities = {
  acceptedInputModalities: ["text", "image", "audio"],
  emittedOutputModalities: ["text"],
  tools: true,
  reasoningReplay: "none",
  mediaForms: { imageUrl: true, imageBase64: true, audioBase64: true },
}

export interface GeminiRequestPlan {
  modelParams: ModelParams & Record<string, unknown>
  request: GenerateContentRequest
}

export interface GeminiStreamState {
  readonly input: CanonicalAdapterInput
  readonly toolCalls: Array<{ name: string; args: Record<string, unknown> }>
}

// Google Generate Content streams response chunks, while @google/generative-ai 0.24.1 exposes
// a separate promise for the aggregated response. Candidate finishReason and aggregate usage are
// therefore decoded only by finishStream.
// Source: https://ai.google.dev/api/generate-content#method:-models.streamgeneratecontent
// Source: https://github.com/google-gemini/deprecated-generative-ai-js/blob/v0.24.1/types/responses.ts
function parseArguments(argumentsJson: string): Record<string, unknown> {
  try {
    const value = JSON.parse(argumentsJson) as unknown
    return value && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {}
  } catch {
    return {}
  }
}

function toolName(callId: string, messages: readonly CanonicalMessage[]): string {
  for (let index = messages.length - 1; index >= 0; index--) {
    const match = messages[index].toolCalls?.find(call => call.id === callId)
    if (match) return match.name
  }
  return callId
}

function toolResultPart(
  result: CanonicalToolResult,
  messages: readonly CanonicalMessage[],
): Part {
  return {
    functionResponse: {
      name: toolName(result.callId, messages),
      response: { output: projectToolOutputToText(result.blocks) },
    },
  }
}

function contentPart(block: Exclude<CanonicalMessage["blocks"][number], CanonicalToolResult>): Part | undefined {
  if (block.type === "text") return block.text ? { text: block.text } : undefined
  if (block.type === "image" || block.type === "audio") {
    if (block.source.kind === "base64") {
      return {
        inlineData: {
          mimeType: block.mediaType ?? (block.type === "image" ? "image/png" : "audio/wav"),
          data: block.source.data,
        },
      }
    }
    if (block.source.kind === "url") {
      return {
        fileData: {
          mimeType: block.mediaType ?? (block.type === "image" ? "image/png" : "audio/wav"),
          fileUri: block.source.url,
        },
      }
    }
  }
  if (block.type === "video" || block.type === "file") {
    throw new ProtocolResponseError("gemini", `cannot serialize ${block.type}`)
  }
  throw new ProtocolResponseError("gemini", `cannot serialize ${block.type} source`)
}

export function canonicalGeminiContents(context: CanonicalAdapterInput["context"]): Content[] {
  const messages = context.stateTurn ? [...context.turns, context.stateTurn] : context.turns
  const contents: Content[] = []
  for (const message of messages) {
    const parts: Part[] = []
    for (const call of message.toolCalls ?? []) {
      parts.push({
        functionCall: { name: call.name, args: parseArguments(call.arguments) },
      })
    }
    for (const block of message.blocks) {
      if (block.type === "tool_result") {
        parts.push(toolResultPart(block, messages))
      } else {
        const part = contentPart(block)
        if (part) parts.push(part)
      }
    }
    if (!parts.length) continue
    contents.push({
      role: message.role === "assistant" ? "model" : "user",
      parts,
    })
  }
  return contents
}

function buildTools(tools: readonly ToolSchema[]): Tool[] {
  if (!tools.length) return []
  return [{
    functionDeclarations: tools.map(tool => ({
      name: tool.name,
      description: tool.description,
      parameters: JSON.parse(tool.parameters),
    })),
  }]
}

function modelExtensions(extensions: Readonly<Record<string, unknown>>): Record<string, unknown> {
  const {
    model: _model,
    systemInstruction: _systemInstruction,
    tools: _tools,
    google_search: _googleSearch,
    response_mime_type: _responseMimeType,
    response_schema: _responseSchema,
    generationConfig: _generationConfig,
    ...rest
  } = extensions
  return rest
}

export function geminiVendorConfig(
  extensions: Readonly<Record<string, unknown>>,
): { tools?: Tool[]; generationConfig?: Record<string, unknown> } {
  const tools: Tool[] = []
  if (extensions.google_search) {
    tools.push({
      googleSearch: typeof extensions.google_search === "object"
        ? extensions.google_search as Record<string, unknown>
        : {},
    } as Tool)
  }
  const generationConfig: Record<string, unknown> = {
    ...(extensions.generationConfig as Record<string, unknown> | undefined),
  }
  if (extensions.response_mime_type != null) {
    generationConfig.responseMimeType = extensions.response_mime_type
  }
  if (extensions.response_schema != null) {
    generationConfig.responseSchema = extensions.response_schema
  }
  return {
    ...(tools.length ? { tools } : {}),
    ...(Object.keys(generationConfig).length ? { generationConfig } : {}),
  }
}

function decodeParts(raw: GenerateContentResponse): { content: string; toolCalls: ToolCall[] } {
  const candidate = raw.candidates?.[0]
  let content = ""
  const toolCalls: ToolCall[] = []
  for (const part of candidate?.content.parts ?? []) {
    if (part.text) content += part.text
    else if (part.functionCall) {
      const call = normalizeToolCall(
        part.functionCall.name,
        part.functionCall.name,
        part.functionCall.args,
      )
      if (call) toolCalls.push(call)
    }
  }
  return { content, toolCalls }
}

function numberField(
  raw: Record<string, unknown>,
  field: string,
): number | undefined {
  const value = raw[field]
  if (value === undefined) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new ProtocolResponseError("gemini", `usage.${field} must be a non-negative finite number`)
  }
  return value
}

export class GeminiAdapter implements ProtocolAdapter<
  GeminiRequestPlan,
  GenerateContentResponse,
  GenerateContentResponse,
  GeminiStreamState,
  GenerateContentResponse
> {
  readonly protocol = "gemini" as const
  readonly protocolCapabilities = GEMINI_PROTOCOL_CAPABILITIES

  buildRequest(input: CanonicalAdapterInput): GeminiRequestPlan {
    const extensions = input.extensions
    const vendor = geminiVendorConfig(extensions)
    const tools = [...buildTools(input.tools), ...(vendor.tools ?? [])]
    return {
      modelParams: {
        ...modelExtensions(extensions),
        model: input.resolved.identity.modelId,
        ...(input.context.systemText ? { systemInstruction: input.context.systemText } : {}),
        ...(tools.length ? { tools } : {}),
        ...(vendor.generationConfig ? { generationConfig: vendor.generationConfig } : {}),
      },
      request: { contents: canonicalGeminiContents(input.context) },
    }
  }

  decodeComplete(raw: GenerateContentResponse, _input: AdapterDecodeInput): { message: Message } {
    const decoded = decodeParts(raw)
    const usage = this.normalizeUsage(raw.usageMetadata)
    const rawUsage = raw.usageMetadata as unknown as Record<string, unknown> | undefined
    const tokenCount = usage?.outputTokens
      ?? (rawUsage ? numberField(rawUsage, "totalTokenCount") : undefined)
    return {
      message: {
        role: "assistant",
        content: decoded.content,
        ...(tokenCount !== undefined ? { tokenCount } : {}),
        toolCalls: decoded.toolCalls,
      },
    }
  }

  createStreamState(input: AdapterStreamInput): GeminiStreamState {
    return { input: input.input, toolCalls: [] }
  }

  pushStreamChunk(chunk: GenerateContentResponse, state: GeminiStreamState): AdapterOutput {
    const events: StreamEvent[] = []
    for (const part of chunk.candidates?.[0]?.content.parts ?? []) {
      if (part.text) events.push({ type: "text_delta", delta: part.text } as TextDelta)
      else if (part.functionCall) {
        state.toolCalls.push({
          name: part.functionCall.name,
          args: part.functionCall.args as Record<string, unknown>,
        })
      }
    }
    return { events }
  }

  finishStream(state: GeminiStreamState, final: GenerateContentResponse): AdapterOutput {
    const events: StreamEvent[] = state.toolCalls.map((call, index) => ({
      type: "tool_call",
      id: `call_${index + 1}`,
      name: call.name,
      arguments: call.args,
    }))
    const usage = this.normalizeUsage(final.usageMetadata)
    const rawStopReason = final.candidates?.[0]?.finishReason
    const stopReason = this.normalizeStopReason(rawStopReason)
    if (usage) {
      const raw = final.usageMetadata as unknown as Record<string, unknown>
      const totalTokens = numberField(raw, "totalTokenCount")
        ?? usage.inputTokens + usage.outputTokens
      events.push({
        type: "usage",
        totalTokens,
        inputTokens: usage.inputTokens,
        outputTokens: usage.outputTokens,
        ...(usage.cacheReadInputTokens
          ? { cacheReadInputTokens: usage.cacheReadInputTokens }
          : {}),
        providerUsage: usage,
        ...(stopReason ? { stopReason } : {}),
        ...(rawStopReason ? { rawStopReason } : {}),
      } as UsageEvent)
    }
    return { events }
  }

  normalizeUsage(raw: unknown): ProviderUsage | undefined {
    if (raw === undefined || raw === null) return undefined
    if (typeof raw !== "object" || Array.isArray(raw)) {
      throw new ProtocolResponseError("gemini", "usage must be an object")
    }
    const usage = raw as Record<string, unknown>
    const inputTokens = numberField(usage, "promptTokenCount")
    const outputTokens = numberField(usage, "candidatesTokenCount")
    numberField(usage, "totalTokenCount")
    const cacheReadInputTokens = numberField(usage, "cachedContentTokenCount")
    if (
      inputTokens === undefined
      && outputTokens === undefined
      && cacheReadInputTokens === undefined
    ) return undefined
    return {
      inputTokens: inputTokens ?? 0,
      outputTokens: outputTokens ?? 0,
      ...(cacheReadInputTokens ? { cacheReadInputTokens } : {}),
    }
  }

  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined {
    if (raw === undefined) return undefined
    switch (raw) {
      case "STOP":
      case "FINISH_REASON_STOP":
        return "end_turn"
      case "MAX_TOKENS":
        return "max_tokens"
      case "SAFETY":
      case "RECITATION":
      case "BLOCKLIST":
      case "PROHIBITED_CONTENT":
      case "SPII":
        return "content_filter"
      default:
        return "other"
    }
  }
}
