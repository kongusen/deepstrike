export interface TextPart {
  type: "text"
  text: string
}

export interface ImagePart {
  type: "image"
  /** Remote image URL (mutually exclusive with `data`). */
  url?: string
  /** Raw base64-encoded image bytes (mutually exclusive with `url`). */
  data?: string
  /** MIME type, e.g. `"image/png"`. Required when `data` is set. */
  mediaType?: string
  /** OpenAI vision detail level. */
  detail?: "auto" | "low" | "high"
}

export interface AudioPart {
  type: "audio"
  /** Raw base64-encoded audio bytes. */
  data: string
  /** MIME type, e.g. `"audio/wav"`. */
  mediaType: string
}

export type ContentPart = TextPart | ImagePart | AudioPart

export interface Message {
  role: "system" | "user" | "assistant" | "tool"
  /** Plain-text content. When `contentParts` is present, this holds only the text segments. */
  content: string
  /** Structured multimodal content. When present, takes precedence over `content` for provider calls. */
  contentParts?: ContentPart[]
  tokenCount?: number
  toolCalls?: ToolCall[]
}

export interface ToolCall {
  id: string
  name: string
  arguments: string // JSON-encoded
}

export interface ToolResult {
  callId: string
  output: string
  isError: boolean
  tokenCount?: number
}

export interface ToolSchema {
  name: string
  description: string
  parameters: string // JSON-encoded JSON Schema
}

export interface StreamEvent {
  type: string
}

export interface TextDelta extends StreamEvent {
  type: "text_delta"
  delta: string
}

export interface ThinkingDelta extends StreamEvent {
  type: "thinking_delta"
  delta: string
}

export interface ToolCallEvent extends StreamEvent {
  type: "tool_call"
  id: string
  name: string
  arguments: Record<string, unknown>
}

export interface ToolResultEvent extends StreamEvent {
  type: "tool_result"
  callId: string
  name: string
  content: string
  isError: boolean
}

export interface DoneEvent extends StreamEvent {
  type: "done"
  iterations: number
  totalTokens: number
  status: string
}

export interface ErrorEvent extends StreamEvent {
  type: "error"
  message: string
}

export interface PermissionRequestEvent extends StreamEvent {
  type: "permission_request"
  callId: string
  toolName: string
  arguments: string
  reason: string
}

export interface TokenUsage {
  inputTokens: number
  outputTokens: number
  totalTokens: number
}

export interface ProviderToolSpec {
  name: string
  description: string
  parameters: Record<string, unknown>
}

export interface RetryConfig {
  maxRetries?: number
  baseDelay?: number
  circuitOpenAfter?: number
  circuitResetAfter?: number
}

export interface LLMProvider {
  complete(messages: Message[], tools: ToolSchema[]): Promise<Message>
  stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent>
}
