/** Provider-neutral durable content ABI.
 *
 * This is deliberately separate from provider-facing ContentPart. Durable records carry only
 * portable content and explicit payload locators; protocol adapters decide how to serialize it.
 */

export class DurableContentError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "DurableContentError"
  }
}

export type DurableSource =
  | { kind: "url"; url: string }
  | { kind: "base64"; data: string }
  | { kind: "file_id"; id: string; affinity: { provider_id: string; endpoint_id: string } }
  | { kind: "object"; handle: string; owner: string; payload_ref: string }

export type DurableContentBlock =
  | { type: "text"; text: string }
  | { type: "image" | "audio" | "video" | "file"; source: DurableSource; media_type?: string; provider_options?: Record<string, unknown> }

export interface DurableContent {
  blocks: DurableContentBlock[]
}

export interface DurableToolResult extends DurableContent {
  call_id: string
  is_error: boolean
}

import type { ToolOutputBlock } from "../types.js"

const CONTENT_KEYS = new Set(["blocks"])
const TOOL_RESULT_KEYS = new Set(["call_id", "is_error", "blocks"])
const SOURCE_KEYS: Record<DurableSource["kind"], Set<string>> = {
  url: new Set(["kind", "url"]),
  base64: new Set(["kind", "data"]),
  file_id: new Set(["kind", "id", "affinity"]),
  object: new Set(["kind", "handle", "owner", "payload_ref"]),
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new DurableContentError(`${label} must be an object`)
  return value as Record<string, unknown>
}

function exactKeys(value: Record<string, unknown>, allowed: Set<string>, label: string): void {
  for (const key of Object.keys(value)) if (!allowed.has(key)) throw new DurableContentError(`${label} has unknown field ${key}`)
}

function requiredString(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0) throw new DurableContentError(`${label} must be a non-empty string`)
  return value
}

function requiredBoolean(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new DurableContentError(`${label} must be a boolean`)
  return value
}

function base64(value: unknown, label: string): string {
  const data = requiredString(value, label)
  if (data.length % 4 !== 0 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data)) {
    throw new DurableContentError(`${label} is not valid base64`)
  }
  return data
}

function source(value: unknown, label: string): DurableSource {
  const raw = object(value, `${label} source`)
  const kind = raw.kind
  if (kind !== "url" && kind !== "base64" && kind !== "file_id" && kind !== "object") throw new DurableContentError(`${label} source kind is invalid`)
  exactKeys(raw, SOURCE_KEYS[kind], `${label} source`)
  if (kind === "url") return { kind, url: requiredString(raw.url, `${label} URL`) }
  if (kind === "base64") return { kind, data: base64(raw.data, `${label} base64 data`) }
  if (kind === "file_id") {
    const id = requiredString(raw.id, `${label} file id`)
    const a = object(raw.affinity, `${label} affinity`)
    exactKeys(a, new Set(["provider_id", "endpoint_id"]), `${label} affinity`)
    return {
      kind,
      id,
      affinity: {
        provider_id: requiredString(a.provider_id, `${label} affinity provider_id`),
        endpoint_id: requiredString(a.endpoint_id, `${label} affinity endpoint_id`),
      },
    }
  }
  return {
    kind,
    handle: requiredString(raw.handle, `${label} object handle`),
    owner: requiredString(raw.owner, `${label} object owner`),
    payload_ref: requiredString(raw.payload_ref, `${label} payload_ref`),
  }
}

function block(value: unknown, index: number): DurableContentBlock {
  const raw = object(value, `content block ${index}`)
  const type = raw.type
  if (type === "text") {
    exactKeys(raw, new Set(["type", "text"]), `content block ${index}`)
    return { type, text: typeof raw.text === "string" ? raw.text : (() => { throw new DurableContentError(`content block ${index} text must be a string`) })() }
  }
  if (type === "tool_result") throw new DurableContentError("nested tool_result blocks are forbidden")
  if (type !== "image" && type !== "audio" && type !== "video" && type !== "file") throw new DurableContentError(`unknown content block type: ${String(type)}`)
  exactKeys(raw, new Set(["type", "source", "media_type", "provider_options"]), `content block ${index}`)
  const mediaType = raw.media_type === undefined ? undefined : requiredString(raw.media_type, `content block ${index} media_type`)
  let providerOptions: Record<string, unknown> | undefined
  if (raw.provider_options !== undefined) providerOptions = object(raw.provider_options, `content block ${index} provider_options`)
  return {
    type,
    source: source(raw.source, `content block ${index}`),
    ...(mediaType === undefined ? {} : { media_type: mediaType }),
    ...(providerOptions === undefined ? {} : { provider_options: providerOptions }),
  }
}

function blocks(value: unknown): DurableContentBlock[] {
  if (!Array.isArray(value)) throw new DurableContentError("content blocks must be an array")
  return value.map(block)
}

export function decodeDurableContent(value: unknown): DurableContent {
  const raw = object(value, "durable content")
  exactKeys(raw, CONTENT_KEYS, "durable content")
  return { blocks: blocks(raw.blocks) }
}

export function decodeDurableToolResult(value: unknown): DurableToolResult {
  const raw = object(value, "durable tool result")
  exactKeys(raw, TOOL_RESULT_KEYS, "durable tool result")
  return { call_id: requiredString(raw.call_id, "tool result call_id"), is_error: requiredBoolean(raw.is_error, "tool result is_error"), blocks: blocks(raw.blocks) }
}

export function encodeDurableContent(content: DurableContent): Record<string, unknown> {
  const decoded = decodeDurableContent(content)
  return { blocks: decoded.blocks }
}

export function encodeDurableToolResult(result: DurableToolResult): Record<string, unknown> {
  const decoded = decodeDurableToolResult(result)
  return { call_id: decoded.call_id, is_error: decoded.is_error, blocks: decoded.blocks }
}

export function toolOutputBlocksToDurable(blocks: readonly ToolOutputBlock[]): DurableContentBlock[] {
  return blocks.map((part, index) => {
    if (part.type === "text") return { type: "text", text: part.text }
    const source = part.source
    const durableSource = source.kind === "fileId"
      ? {
          kind: "file_id" as const,
          id: source.id,
          affinity: source.affinity
            ? { provider_id: source.affinity.providerId, endpoint_id: source.affinity.endpointId }
            : (() => { throw new DurableContentError(`tool output block ${index} fileId source requires endpoint affinity`) })(),
        }
      : source.kind === "base64" ? { kind: "base64" as const, data: source.data }
      : source.kind === "url" ? { kind: "url" as const, url: source.url }
      : {
          kind: "object" as const,
          handle: source.handle,
          owner: source.owner ?? (() => { throw new DurableContentError(`tool output block ${index} object source requires owner`) })(),
          payload_ref: source.payloadRef ?? (() => { throw new DurableContentError(`tool output block ${index} object source requires payloadRef`) })(),
        }
    return { type: part.type, source: durableSource, ...(part.mediaType ? { media_type: part.mediaType } : {}), ...(part.providerOptions ? { provider_options: part.providerOptions } : {}) }
  })
}

export function durableBlocksToToolOutput(blocks: readonly DurableContentBlock[]): ToolOutputBlock[] {
  return blocks.map((part) => {
    if (part.type === "text") return part
    const source = part.source
    const sdkSource = source.kind === "file_id"
      ? { kind: "fileId" as const, id: source.id, affinity: { providerId: source.affinity.provider_id, endpointId: source.affinity.endpoint_id } }
      : source.kind === "base64" ? source
      : source.kind === "url" ? source
      : { kind: "object" as const, handle: source.handle, owner: source.owner, payloadRef: source.payload_ref }
    return { type: part.type, source: sdkSource, ...(part.media_type ? { mediaType: part.media_type } : {}), ...(part.provider_options ? { providerOptions: part.provider_options } : {}) } as ToolOutputBlock
  })
}
