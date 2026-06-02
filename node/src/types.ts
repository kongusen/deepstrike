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

export interface ToolResultPart {
  type: "tool_result"
  callId: string
  output: string
  isError: boolean
}

export type ContentPart = TextPart | ImagePart | AudioPart | ToolResultPart

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

export type ToolErrorKind =
  | "recoverable"
  | "fatal"
  | "governance_denied"
  | "provider_failure"
  | "timeout"
  | "user_interrupt"

export interface ToolResult {
  callId: string
  output: string
  isError: boolean
  isFatal?: boolean
  errorKind?: ToolErrorKind
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

export type ToolChunk =
  | string
  | { type: "text"; text: string }
  | { type: "progress"; progress: number; message?: string }
  | { type: "artifact"; artifactId: string; mimeType?: string; label?: string }
  | { type: "json_patch"; patch: Record<string, unknown> }
  | { type: "suspend"; suspensionId: string; payload?: Record<string, unknown> }

export interface ToolDeltaEvent extends StreamEvent {
  type: "tool_delta"
  callId: string
  name: string
  /** Backward-compatible text projection when the chunk carries text. */
  delta?: string
  chunk: Exclude<ToolChunk, string>
}

export interface ToolSuspendEvent extends StreamEvent {
  type: "tool_suspend"
  callId: string
  name: string
  suspensionId: string
  payload?: Record<string, unknown>
}

export interface ToolResultEvent extends StreamEvent {
  type: "tool_result"
  callId: string
  name: string
  content: string
  isError: boolean
  isFatal?: boolean
  errorKind?: ToolErrorKind
}

export interface DoneEvent extends StreamEvent {
  type: "done"
  iterations: number
  totalTokens: number
  status: string
  dreamResult?: import("./memory/protocols.js").DreamResult
}

export interface ErrorEvent extends StreamEvent {
  type: "error"
  message: string
}

export interface ToolArgumentRepairedEvent extends StreamEvent {
  type: "tool_argument_repaired"
  callId: string
  name: string
  originalArguments: string
  repairedArguments: string
}

export interface PermissionRequestEvent extends StreamEvent {
  type: "permission_request"
  callId: string
  toolName: string
  arguments: string
  reason: string
}

export interface PermissionResponse {
  approved: boolean
  responder?: string
  reason?: string
}

export interface PermissionResolvedEvent extends StreamEvent {
  type: "permission_resolved"
  callId: string
  toolName: string
  approved: boolean
  responder: string
  reason?: string
}

export interface ToolDeniedEvent extends StreamEvent {
  type: "tool_denied"
  callId: string
  toolName: string
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

/**
 * Opaque provider-owned state scoped to a single Agent run.
 *
 * The framework only creates and threads this object through provider turns.
 * Providers may use it for protocol-native continuation state such as
 * Responses `previous_response_id` without leaking those semantics into the kernel.
 */
export type ProviderRunState = Record<string, unknown>

/** Provider-native fields required to replay a turn across requests (thinking blocks, reasoning_content, etc.). */
export interface ProviderReplay {
  /** Anthropic-style assistant content blocks (thinking, text, tool_use). */
  native_blocks?: Array<Record<string, unknown>>
  /** OpenAI-compatible reasoning field (DeepSeek, etc.). */
  reasoning_content?: string
}

/** Structured render output produced by the kernel for each LLM call. */
export interface RenderedContext {
  /** Identity + Knowledge combined — for providers with a single system slot (OpenAI). */
  systemText: string
  /** Identity only (system partition). Anthropic system[0] with cache_control. */
  systemStable?: string
  /** Knowledge (memory retrievals, skill definitions, artifacts). Anthropic system[1] with cache_control. */
  systemKnowledge?: string
  /** Turns: [0] = State (task_state + signals), [1..N] = History. */
  turns: Message[]
}

/**
 * Runtime execution policy advertised by a provider.
 * RuntimeRunner merges these with RuntimeOptions — explicit options always win.
 */
export interface RuntimePolicy {
  /** Maximum agent turns before termination. */
  maxTurns?: number
  /** Per-run wall-clock timeout in ms. */
  timeoutMs?: number
}

export interface LLMProvider {
  createRunState?(): ProviderRunState
  /**
   * Optional: return the recommended runtime policy for this provider's model.
   * RuntimeRunner uses this as a fallback when the caller has not specified
   * maxTurns / timeoutMs in RuntimeOptions.
   */
  runtimePolicy?(): RuntimePolicy
  /** Read provider-native replay fields captured after the most recent assistant turn. */
  peekProviderReplay?(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined
  /** Restore provider-native replay fields when rebuilding history from SessionLog. */
  seedProviderReplay?(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void
  complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message>
  stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
  ): AsyncIterable<StreamEvent>
}

/**
 * Optional async summarizer called after context compression.
 * Produces a richer LLM-generated summary that replaces the rule-based one on next wake.
 */
export interface AsyncSummarizer {
  summarize(archived: Message[], action: string): Promise<string>
}

/**
 * Long-term memory summarizer for semantic `page_out` events (Layer 5 contract).
 * The kernel emits `page_out { tier_hint: "semantic" }`; the SDK persists an LLM summary to DreamStore.
 */
export interface DreamSummarizer {
  summarize(archived: Message[], context: { action?: string }): Promise<string>
}

export interface TaskUpdate {
  plan?: string[]
  currentStep?: number
  progress?: string
  scratchpad?: string
  blockedOn?: string[]
  preservedRefs?: string[]
}
