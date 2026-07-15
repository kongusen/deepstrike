// Shared types — identical shape to Node SDK, camelCase throughout

import type { WorkflowNodeSpec } from "./runtime/types/agent.js"

export interface ContentPart {
  type: "text" | "image" | "audio" | "tool_result"
  text?: string
  /** Remote image URL (mutually exclusive with `data`). */
  url?: string
  /** Raw base64-encoded bytes (image/audio). */
  data?: string
  /** MIME type, e.g. `"image/png"`. */
  mediaType?: string
  /** OpenAI vision detail level. */
  detail?: "auto" | "low" | "high"
  callId?: string
  output?: string
  isError?: boolean
}

export interface Message {
  role: "system" | "user" | "assistant" | "tool"
  content: string
  tokenCount?: number
  toolCalls?: ToolCall[]
  /** Multimodal parts (text + image/audio). When present, providers render these
   *  instead of the plain `content` string. */
  contentParts?: ContentPart[]
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

/** Structured provider context from the kernel (`call_llm` action). */
export interface RenderedContext {
  systemText: string
  systemStable?: string
  systemKnowledge?: string
  turns: Message[]
  /** Volatile State turn (task_state + signals), rendered after the cacheable
   *  history. Absent on un-rebuilt bindings — then it's still inside turns[0]. */
  stateTurn?: Message
  /** Message count of the frozen history prefix (compaction boundary). When set,
   *  Anthropic pins a deep cache breakpoint here instead of the rolling pair. */
  frozenPrefixLen?: number
  budgetOverflow?: ContextBudgetOverflow
}

export interface ContextBudgetOverflow {
  kind: "fixed_context" | "protected_tail"
  requiredTokens: number
  maxTokens: number
}

export interface StreamEvent { type: string }
export interface TextDelta extends StreamEvent { type: "text_delta"; delta: string }
export interface UsageEvent extends StreamEvent { type: "usage"; totalTokens: number; inputTokens?: number; outputTokens?: number; cacheReadInputTokens?: number; cacheCreationInputTokens?: number; cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number }; /** Provider stop reason — `max_tokens` (Anthropic) / `length` (OpenAI) flag an output-cap truncation driving the kernel's max-output-tokens recovery. */ stopReason?: string }
export interface ThinkingDelta extends StreamEvent { type: "thinking_delta"; delta: string }
export interface ToolCallEvent extends StreamEvent { type: "tool_call"; id: string; name: string; arguments: Record<string, unknown> }
export interface ToolResultEvent extends StreamEvent { type: "tool_result"; callId: string; name: string; content: string; isError: boolean; isFatal?: boolean; errorKind?: ToolErrorKind }
/** R3-1: a workflow node's agent called `submit_workflow_nodes`; the runner surfaces the requested nodes (the workflow lives in the parent kernel) and `runWorkflow` sends them to the parent kernel. */
export interface WorkflowNodesSubmittedEvent extends StreamEvent { type: "workflow_nodes_submitted"; nodes: WorkflowNodeSpec[] }
export interface DoneEvent extends StreamEvent { type: "done"; iterations: number; totalTokens: number; status: string; /** ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs). */ paceDecision?: import("./runtime/kernel-step.js").PaceDecision }
export interface ErrorEvent extends StreamEvent { type: "error"; message: string }
export interface PermissionRequestEvent extends StreamEvent { type: "permission_request"; callId: string; toolName: string; arguments: string; reason: string }
export interface PermissionResponse { approved: boolean; responder?: string; reason?: string }
export interface PermissionResolvedEvent extends StreamEvent {
  type: "permission_resolved"
  callId: string
  toolName: string
  approved: boolean
  responder: string
  reason?: string
}
export interface ToolDeniedEvent extends StreamEvent { type: "tool_denied"; callId: string; toolName: string; reason: string }
/** A tool's `ctx.audit(label, fn)` best-effort side-effect threw. The tool itself completed
 *  successfully (no isError flip, no retry); this event lets the host log / monitor that an
 *  audit-store / metrics-emit / non-essential persistence step failed. */
export interface ToolAuditFailedEvent extends StreamEvent {
  type: "tool_audit_failed"
  callId: string
  name: string
  label: string
  error: string
}
export interface ToolArgumentRepairedEvent extends StreamEvent {
  type: "tool_argument_repaired"
  callId: string
  name: string
  originalArguments: string
  repairedArguments: string
}

/** Kernel session-entropy measurement at a completed turn boundary (see the Node SDK's
 *  `EntropySample` for the canonical documentation; this is the WASM mirror). */
export interface EntropySample {
  turn: number
  score: number
  scoreVersion: number
  rho: number
  repeatPressure: number
  failureRate: number
  rollbacksInWindow: number
  windowTurns: number
}

/** One kernel entropy sample, emitted once per completed turn (a heartbeat watch source). */
export interface EntropySampleEvent extends StreamEvent {
  type: "entropy_sample"
  sample: EntropySample
}

/** The opt-in kernel entropy watch tripped (see `RunnerOptions.entropyWatch`). */
export interface EntropyAlertEvent extends StreamEvent {
  type: "entropy_alert"
  turn: number
  score: number
  threshold: number
}

/** Opt-in kernel-side threshold watch over the per-turn entropy score (Node SDK mirror). */
export interface EntropyWatchOptions {
  enabled?: boolean
  threshold?: number
  hysteresis?: number
  cooldownTurns?: number
  notifyModel?: boolean
}

/**
 * Opaque per-run state owned by the provider (e.g. OpenAI Responses continuation).
 * The framework creates and threads this object; providers may read/write it.
 */
export type ProviderRunState = Record<string, unknown>

export type ProviderProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "gemini"

/**
 * Cache_control placement strategy for the Anthropic protocol. Pass via
 * `extensions.cacheBreakpointStrategy` on any provider call (the runner threads
 * `RuntimeOptions.extensions` through automatically). See the Node SDK's
 * `CacheBreakpointStrategy` for the canonical documentation; this is the WASM mirror.
 */
export type CacheBreakpointStrategy =
  | "default"
  | "tools-only"
  | "system-only"
  | "frozen-prefix"
  | "none"

export interface ProviderDescriptor {
  provider: string
  protocol: ProviderProtocol
  model: string
  reasoning: {
    supported: boolean
    preserveAcrossToolTurns: boolean
    requiresReplayForToolTurns?: boolean
  }
  toolCalls: {
    supported: boolean
    requiresStrictPairing: boolean
  }
}

export interface ProviderReplay {
  schema_version?: 1 | 2
  provider?: string
  protocol?: ProviderProtocol
  model?: string
  native_blocks?: Array<Record<string, unknown>>
  reasoning_content?: string
  reasoning_details?: unknown
  native_message?: unknown
  tool_calls?: unknown[]
}

export interface LLMProvider {
  createRunState?(): ProviderRunState
  runtimePolicy?(): { maxTurns?: number; timeoutMs?: number }
  descriptor?(): ProviderDescriptor
  peekProviderReplay?(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined
  seedProviderReplay?(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void
  complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message>
  stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
    /** #2-B-ii: when provided, a preempt (`interrupt()`) aborts the in-flight request. SDK-client
     *  providers forward it via `{ signal }`; the runner also breaks the consume loop on abort, so
     *  providers that ignore it still stop processing immediately. Optional ⇒ backward-compatible. */
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent>
}

export interface DreamSummarizer {
  summarize(archived: Message[], context: { action?: string }): Promise<string>
}
