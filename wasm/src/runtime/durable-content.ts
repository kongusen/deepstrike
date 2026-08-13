export class DurableContentError extends Error {
  constructor(message: string) { super(message); this.name = "DurableContentError" }
}

export type DurableSource =
  | { kind: "url"; url: string }
  | { kind: "base64"; data: string }
  | { kind: "file_id"; id: string; affinity: { provider_id: string; endpoint_id: string } }
  | { kind: "object"; handle: string; owner: string; payload_ref: string }

export type DurableContentBlock =
  | { type: "text"; text: string }
  | { type: "image" | "audio" | "video" | "file"; source: DurableSource; media_type?: string; provider_options?: Record<string, unknown> }

export interface DurableContent { blocks: DurableContentBlock[] }
export interface DurableToolResult extends DurableContent { call_id: string; is_error: boolean }

function obj(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new DurableContentError(`${label} must be an object`)
  return value as Record<string, unknown>
}
function keys(value: Record<string, unknown>, allowed: string[], label: string): void {
  for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new DurableContentError(`${label} has unknown field ${key}`)
}
function string(value: unknown, label: string): string {
  if (typeof value !== "string" || !value) throw new DurableContentError(`${label} must be a non-empty string`)
  return value
}
function boolean(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new DurableContentError(`${label} must be a boolean`)
  return value
}
function base64(value: unknown, label: string): string {
  const data = string(value, label)
  if (data.length % 4 !== 0 || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data)) throw new DurableContentError(`${label} is not valid base64`)
  return data
}
function source(value: unknown, label: string): DurableSource {
  const raw = obj(value, `${label} source`)
  const kind = raw.kind
  if (kind === "url") { keys(raw, ["kind", "url"], `${label} source`); return { kind, url: string(raw.url, `${label} URL`) } }
  if (kind === "base64") { keys(raw, ["kind", "data"], `${label} source`); return { kind, data: base64(raw.data, `${label} base64 data`) } }
  if (kind === "file_id") {
    keys(raw, ["kind", "id", "affinity"], `${label} source`)
    const a = obj(raw.affinity, `${label} affinity`); keys(a, ["provider_id", "endpoint_id"], `${label} affinity`)
    return { kind, id: string(raw.id, `${label} file id`), affinity: { provider_id: string(a.provider_id, `${label} affinity provider_id`), endpoint_id: string(a.endpoint_id, `${label} affinity endpoint_id`) } }
  }
  if (kind === "object") {
    keys(raw, ["kind", "handle", "owner", "payload_ref"], `${label} source`)
    return { kind, handle: string(raw.handle, `${label} object handle`), owner: string(raw.owner, `${label} object owner`), payload_ref: string(raw.payload_ref, `${label} payload_ref`) }
  }
  throw new DurableContentError(`${label} source kind is invalid`)
}
function block(value: unknown, index: number): DurableContentBlock {
  const raw = obj(value, `content block ${index}`)
  if (raw.type === "text") { keys(raw, ["type", "text"], `content block ${index}`); return { type: "text", text: string(raw.text, `content block ${index} text`) } }
  if (raw.type === "tool_result") throw new DurableContentError("nested tool_result blocks are forbidden")
  if (raw.type !== "image" && raw.type !== "audio" && raw.type !== "video" && raw.type !== "file") throw new DurableContentError(`unknown content block type: ${String(raw.type)}`)
  keys(raw, ["type", "source", "media_type", "provider_options"], `content block ${index}`)
  const result: DurableContentBlock = { type: raw.type, source: source(raw.source, `content block ${index}`) }
  if (raw.media_type !== undefined) result.media_type = string(raw.media_type, `content block ${index} media_type`)
  if (raw.provider_options !== undefined) result.provider_options = obj(raw.provider_options, `content block ${index} provider_options`)
  return result
}
function blocks(value: unknown): DurableContentBlock[] { if (!Array.isArray(value)) throw new DurableContentError("content blocks must be an array"); return value.map(block) }

export function decodeDurableContent(value: unknown): DurableContent {
  const raw = obj(value, "durable content"); keys(raw, ["blocks"], "durable content")
  return { blocks: blocks(raw.blocks) }
}
export function decodeDurableToolResult(value: unknown): DurableToolResult {
  const raw = obj(value, "durable tool result")
  keys(raw, ["call_id", "is_error", "blocks"], "durable tool result")
  return { call_id: string(raw.call_id, "tool result call_id"), is_error: boolean(raw.is_error, "tool result is_error"), blocks: blocks(raw.blocks) }
}
export function encodeDurableContent(content: DurableContent): Record<string, unknown> { return decodeDurableContent(content) as unknown as Record<string, unknown> }
export function encodeDurableToolResult(result: DurableToolResult): Record<string, unknown> { return decodeDurableToolResult(result) as unknown as Record<string, unknown> }
export function toolOutputBlocksToDurable(blocks: readonly import("../types.js").ToolOutputBlock[]): DurableContentBlock[] {
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
export function durableBlocksToToolOutput(blocks: readonly DurableContentBlock[]): import("../types.js").ToolOutputBlock[] {
  return blocks.map((part) => {
    if (part.type === "text") return part
    const source = part.source
    const sdkSource = source.kind === "file_id"
      ? { kind: "fileId" as const, id: source.id, affinity: { providerId: source.affinity.provider_id, endpointId: source.affinity.endpoint_id } }
      : source.kind === "base64" ? source : source.kind === "url" ? source : { kind: "object" as const, handle: source.handle, owner: source.owner, payloadRef: source.payload_ref }
    return { type: part.type, source: sdkSource, ...(part.media_type ? { mediaType: part.media_type } : {}), ...(part.provider_options ? { providerOptions: part.provider_options } : {}) } as import("../types.js").ToolOutputBlock
  })
}
