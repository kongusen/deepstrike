// Shared types — identical shape to Node SDK, camelCase throughout

export interface Message {
  role: "system" | "user" | "assistant" | "tool"
  content: string
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

export interface StreamEvent { type: string }
export interface TextDelta extends StreamEvent { type: "text_delta"; delta: string }
export interface ThinkingDelta extends StreamEvent { type: "thinking_delta"; delta: string }
export interface ToolCallEvent extends StreamEvent { type: "tool_call"; id: string; name: string; arguments: Record<string, unknown> }
export interface ToolResultEvent extends StreamEvent { type: "tool_result"; callId: string; name: string; content: string; isError: boolean }
export interface DoneEvent extends StreamEvent { type: "done"; iterations: number; totalTokens: number; status: string }
export interface ErrorEvent extends StreamEvent { type: "error"; message: string }
export interface PermissionRequestEvent extends StreamEvent { type: "permission_request"; callId: string; toolName: string; arguments: string; reason: string }

export interface LLMProvider {
  stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent>
}
