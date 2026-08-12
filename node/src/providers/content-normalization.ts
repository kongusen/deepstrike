import type {
  ContentPart,
  MediaSource,
  Message,
  RenderedContext,
  ToolOutputBlock,
  ToolResultPart,
  ToolSchema,
  ToolCall,
  ProviderReplay,
} from "../types.js"
import type {
  EffectiveModelCapabilities,
  InputModality,
  ResolvedProviderRuntime,
} from "./model-registry.js"
import { requireContentDisposition } from "./content-policy.js"

export class ContentValidationError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "ContentValidationError"
  }
}

export class ToolResultProjectionConflictError extends ContentValidationError {
  constructor(callId: string) {
    super(`Tool result projection conflict for ${callId}: output does not match contentParts`)
    this.name = "ToolResultProjectionConflictError"
  }
}

export interface CanonicalToolResult {
  readonly type: "tool_result"
  readonly callId: string
  readonly blocks: readonly ToolOutputBlock[]
  readonly isError: boolean
  /** Preserves the caller's explicit block-vs-legacy-text wire intent without duplicating content. */
  readonly contentForm?: "legacy_text" | "blocks"
}

export type CanonicalMessageBlock = ToolOutputBlock | CanonicalToolResult

export interface CanonicalMessage {
  readonly role: Message["role"]
  readonly blocks: readonly CanonicalMessageBlock[]
  /** `blocks` remains authoritative; this only preserves an observable wire-shape distinction. */
  readonly contentForm?: "legacy_text" | "blocks"
  readonly toolCalls?: readonly ToolCall[]
  readonly tokenCount?: number
  readonly providerReplay?: ProviderReplay
}

export interface CanonicalRenderedContext {
  readonly systemText: string
  readonly systemStable?: string
  readonly systemKnowledge?: string
  readonly turns: readonly CanonicalMessage[]
  readonly stateTurn?: CanonicalMessage
  readonly frozenPrefixLen?: number
  readonly budgetOverflow?: RenderedContext["budgetOverflow"]
}

export interface CanonicalAdapterInput {
  readonly context: CanonicalRenderedContext
  readonly tools: readonly ToolSchema[]
  readonly resolved: ResolvedProviderRuntime<unknown>
  readonly extensions: Readonly<Record<string, unknown>>
}

function requireNonEmpty(value: unknown, label: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new ContentValidationError(`${label} must be a non-empty string`)
  }
}

function requireBase64(value: unknown, label: string): asserts value is string {
  requireNonEmpty(value, label)
  if (
    value.length % 4 !== 0
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)
  ) {
    throw new ContentValidationError(`${label} is not valid base64`)
  }
}

function validateSource(source: MediaSource, modality: string): void {
  if (!source || typeof source !== "object") {
    throw new ContentValidationError(`${modality} source is required`)
  }
  switch (source.kind) {
    case "url": requireNonEmpty(source.url, `${modality} URL`); return
    case "base64": requireBase64(source.data, `${modality} base64 data`); return
    case "fileId": requireNonEmpty(source.id, `${modality} file id`); return
    case "object": requireNonEmpty(source.handle, `${modality} object handle`); return
    default: throw new ContentValidationError(`${modality} source kind is invalid`)
  }
}

export function validateToolOutputBlocks(blocks: readonly ToolOutputBlock[]): void {
  for (const raw of blocks as readonly unknown[]) {
    if (!raw || typeof raw !== "object") throw new ContentValidationError("tool output block must be an object")
    const block = raw as Record<string, unknown>
    switch (block.type) {
      case "text":
        if (typeof block.text !== "string") throw new ContentValidationError("tool output text must be a string")
        break
      case "image":
      case "audio":
      case "video":
      case "file":
        validateSource(block.source as MediaSource, String(block.type))
        break
      case "tool_result":
        throw new ContentValidationError("nested tool_result blocks are forbidden")
      default:
        throw new ContentValidationError(`unknown tool output block type: ${String(block.type)}`)
    }
  }
}

export function projectToolOutputToText(blocks: readonly ToolOutputBlock[]): string {
  validateToolOutputBlocks(blocks)
  return blocks.map(block => block.type === "text" ? block.text : `[${block.type}]`).join("\n")
}

export function normalizeToolResultPart(part: ToolResultPart): CanonicalToolResult {
  if (part.contentParts === undefined) {
    return {
      type: "tool_result",
      callId: part.callId,
      blocks: [{ type: "text", text: part.output }],
      isError: part.isError,
      contentForm: "legacy_text",
    }
  }
  const projection = projectToolOutputToText(part.contentParts)
  if (projection !== part.output) throw new ToolResultProjectionConflictError(part.callId)
  return {
    type: "tool_result",
    callId: part.callId,
    blocks: part.contentParts,
    isError: part.isError,
    contentForm: "blocks",
  }
}

function attachMessage(
  message: Message,
  overlay: ReadonlyMap<string, readonly ToolOutputBlock[]>,
): Message {
  const needs = message.contentParts?.some(
    part => part.type === "tool_result" && part.contentParts === undefined && overlay.has(part.callId),
  )
  if (!needs) return message
  return {
    ...message,
    contentParts: message.contentParts!.map(part =>
      part.type === "tool_result" && part.contentParts === undefined && overlay.has(part.callId)
        ? { ...part, contentParts: [...overlay.get(part.callId)!] }
        : part,
    ),
  }
}

/** Attach process-local blocks only within the operation that produced them. */
export function attachToolOutputOverlay(
  context: RenderedContext,
  overlay: ReadonlyMap<string, readonly ToolOutputBlock[]>,
): RenderedContext {
  if (overlay.size === 0) return context
  const turns = context.turns.map(message => attachMessage(message, overlay))
  const stateTurn = context.stateTurn ? attachMessage(context.stateTurn, overlay) : undefined
  if (turns.every((turn, index) => turn === context.turns[index]) && stateTurn === context.stateTurn) return context
  return { ...context, turns, ...(stateTurn ? { stateTurn } : {}) }
}

function assertSupported(
  modality: InputModality,
  capabilities: EffectiveModelCapabilities,
  providerId: string,
): void {
  if (capabilities.inputModalities[modality].state === "unsupported") {
    throw new ContentValidationError(`UnsupportedModality: ${modality} is not supported by ${providerId}`)
  }
}

function validateSourceAffinity(
  modality: InputModality,
  source: MediaSource,
  capabilities: EffectiveModelCapabilities,
  resolved: ResolvedProviderRuntime<unknown>,
): void {
  const key = modality === "image"
    ? source.kind === "url" ? "imageUrl" : source.kind === "base64" ? "imageBase64" : source.kind === "fileId" ? "fileId" : undefined
    : modality === "audio"
      ? source.kind === "url" ? "audioUrl" : source.kind === "base64" ? "audioBase64" : source.kind === "fileId" ? "fileId" : undefined
      : source.kind === "fileId" ? "fileId" : undefined
  if (key && capabilities.mediaForms[key].state === "unsupported") {
    throw new ContentValidationError(`Unsupported media source ${source.kind} for ${modality} on ${resolved.identity.providerId}`)
  }
  if (source.kind === "fileId" && source.affinity) {
    if (
      source.affinity.providerId !== resolved.identity.providerId
      || source.affinity.endpointId !== resolved.identity.endpointId
    ) {
      throw new ContentValidationError(
        `Provider file ${source.id} belongs to ${source.affinity.providerId}/${source.affinity.endpointId}, `
        + `not ${resolved.identity.providerId}/${resolved.identity.endpointId}`,
      )
    }
  }
}

function normalizeMessage(
  message: Message,
  replayForMessage?: (message: Message) => ProviderReplay | undefined,
): CanonicalMessage {
  const providerReplay = replayForMessage?.(message)
  const blocks: CanonicalMessageBlock[] = message.contentParts === undefined
    ? [{ type: "text", text: message.content }]
    : message.contentParts.map(part => {
        if (part.type === "text") return { type: "text", text: part.text }
        if (part.type === "image") {
          if ((part.url === undefined) === (part.data === undefined)) {
            throw new ContentValidationError("image requires exactly one of url or data")
          }
          return {
            type: "image",
            source: part.data !== undefined
              ? { kind: "base64", data: part.data }
              : { kind: "url", url: part.url! },
            ...(part.mediaType ? { mediaType: part.mediaType } : {}),
            ...(part.detail ? { providerOptions: { openai_detail: part.detail } } : {}),
          } satisfies ToolOutputBlock
        }
        if (part.type === "audio") {
          return {
            type: "audio",
            source: { kind: "base64", data: part.data },
            mediaType: part.mediaType,
          } satisfies ToolOutputBlock
        }
        return normalizeToolResultPart(part)
      })
  return {
    role: message.role,
    blocks,
    contentForm: message.contentParts === undefined ? "legacy_text" : "blocks",
    ...(message.toolCalls ? { toolCalls: message.toolCalls } : {}),
    ...(message.tokenCount !== undefined ? { tokenCount: message.tokenCount } : {}),
    ...(providerReplay ? { providerReplay } : {}),
  }
}

export function normalizeCanonicalContext(
  context: RenderedContext,
  replayForMessage?: (message: Message) => ProviderReplay | undefined,
): CanonicalRenderedContext {
  return {
    systemText: context.systemText,
    ...(context.systemStable !== undefined ? { systemStable: context.systemStable } : {}),
    ...(context.systemKnowledge !== undefined ? { systemKnowledge: context.systemKnowledge } : {}),
    turns: context.turns.map(message => normalizeMessage(message, replayForMessage)),
    ...(context.stateTurn ? { stateTurn: normalizeMessage(context.stateTurn, replayForMessage) } : {}),
    ...(context.frozenPrefixLen !== undefined ? { frozenPrefixLen: context.frozenPrefixLen } : {}),
    ...(context.budgetOverflow !== undefined ? { budgetOverflow: context.budgetOverflow } : {}),
  }
}

function validateCanonicalMessage(
  message: CanonicalMessage,
  resolved: ResolvedProviderRuntime<unknown>,
): void {
  for (const item of message.blocks) {
    const placement = item.type === "tool_result" ? "tool_result" : "message"
    const blocks = item.type === "tool_result" ? item.blocks : [item]
    validateToolOutputBlocks(blocks)
    for (const block of blocks) {
      if (block.type === "text") continue
      // Protocol policy is the stable user-visible reason for document/video refusal; a model
      // capability cannot make a protocol-level unsupported shape serializable.
      requireContentDisposition(resolved.identity.protocol, block.type, placement)
      assertSupported(block.type, resolved.effectiveCapabilities, resolved.identity.providerId)
      validateSourceAffinity(block.type, block.source, resolved.effectiveCapabilities, resolved)
    }
  }
}

/** Full-tree validation uses the resolved operation capabilities. Unknown stays unknown/fail-open. */
export function validateCanonicalAdapterInput(input: CanonicalAdapterInput): void {
  for (const message of input.context.turns) validateCanonicalMessage(message, input.resolved)
  if (input.context.stateTurn) validateCanonicalMessage(input.context.stateTurn, input.resolved)
}

export function normalizeCanonicalAdapterInput(input: {
  context: RenderedContext
  tools: readonly ToolSchema[]
  resolved: ResolvedProviderRuntime<unknown>
  extensions?: Readonly<Record<string, unknown>>
  replayForMessage?: (message: Message) => ProviderReplay | undefined
}): CanonicalAdapterInput {
  const canonical: CanonicalAdapterInput = {
    context: normalizeCanonicalContext(input.context, input.replayForMessage),
    tools: input.tools,
    resolved: input.resolved,
    extensions: input.extensions ?? {},
  }
  validateCanonicalAdapterInput(canonical)
  return canonical
}

export function normalizeToolResultContent(
  callId: string,
  output: string,
  isError: boolean,
  contentParts: readonly ToolOutputBlock[] | undefined,
): CanonicalToolResult {
  return normalizeToolResultPart({
    type: "tool_result",
    callId,
    output,
    isError,
    ...(contentParts === undefined ? {} : { contentParts: [...contentParts] }),
  })
}
