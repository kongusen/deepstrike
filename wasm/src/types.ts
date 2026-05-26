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

/** Structured provider context from the kernel (`call_llm` action). */
export interface RenderedContext {
  systemText: string
  turns: Message[]
}

export interface StreamEvent { type: string }
export interface TextDelta extends StreamEvent { type: "text_delta"; delta: string }
export interface UsageEvent extends StreamEvent { type: "usage"; totalTokens: number }
export interface ThinkingDelta extends StreamEvent { type: "thinking_delta"; delta: string }
export interface ToolCallEvent extends StreamEvent { type: "tool_call"; id: string; name: string; arguments: Record<string, unknown> }
export interface ToolResultEvent extends StreamEvent { type: "tool_result"; callId: string; name: string; content: string; isError: boolean }
export interface DoneEvent extends StreamEvent { type: "done"; iterations: number; totalTokens: number; status: string }
export interface ErrorEvent extends StreamEvent { type: "error"; message: string }
export interface PermissionRequestEvent extends StreamEvent { type: "permission_request"; callId: string; toolName: string; arguments: string; reason: string }
export interface ToolDeniedEvent extends StreamEvent { type: "tool_denied"; callId: string; toolName: string; reason: string }
export interface ToolArgumentRepairedEvent extends StreamEvent {
  type: "tool_argument_repaired"
  callId: string
  name: string
  originalArguments: string
  repairedArguments: string
}

/**
 * Opaque per-run state owned by the provider (e.g. OpenAI Responses continuation).
 * The framework creates and threads this object; providers may read/write it.
 */
export type ProviderRunState = Record<string, unknown>

export interface ProviderReplay {
  native_blocks?: Array<Record<string, unknown>>
  reasoning_content?: string
}

export interface LLMProvider {
  createRunState?(): ProviderRunState
  runtimePolicy?(): { maxTurns?: number; timeoutMs?: number }
  peekProviderReplay?(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined
  seedProviderReplay?(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void
  complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message>
  stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
  ): AsyncIterable<StreamEvent>
}
