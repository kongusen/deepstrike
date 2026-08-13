import type { WorkflowNodeSpec } from "./types/agent.js"

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
  /** Structured tool output. Provider boundaries normalize this into one canonical block list
   * and reject `output` when it disagrees with the deterministic text projection. */
  contentParts?: ToolOutputBlock[]
}

export type ContentPart = TextPart | ImagePart | AudioPart | ToolResultPart

/**
 * spc_011-B-05: canonical multimodal content, additive alongside `ContentPart` during the
 * migration. `ContentBlockImage`/`ContentBlockAudio`/etc.
 * are distinctly named (not reusing `ImagePart`/`AudioPart`) since those names are already taken
 * by `ContentPart`'s variants with a different shape (`url?/data?` inline vs `source: MediaSource`).
 */
export type MediaSource =
  | { kind: "url"; url: string }
  | { kind: "base64"; data: string }
  | {
      kind: "fileId"
      id: string
      /** Endpoint that issued this provider-owned reference. An omitted affinity constrains the
       * reference to the already-resolved current endpoint. */
      affinity?: { providerId: string; endpointId: string }
    }
  | { kind: "object"; handle: string; owner?: string; payloadRef?: string }

export interface ContentBlockText { type: "text"; text: string }
export interface ContentBlockImage { type: "image"; source: MediaSource; mediaType?: string; providerOptions?: Record<string, unknown> }
export interface ContentBlockAudio { type: "audio"; source: MediaSource; mediaType?: string; providerOptions?: Record<string, unknown> }
export interface ContentBlockVideo { type: "video"; source: MediaSource; mediaType?: string; providerOptions?: Record<string, unknown> }
export interface ContentBlockFile { type: "file"; source: MediaSource; filename?: string; mediaType?: string; providerOptions?: Record<string, unknown> }

/** Legal content returned by a tool. Deliberately excludes ToolResult, so nesting is
 * unrepresentable in the canonical type. */
export type ToolOutputBlock =
  | ContentBlockText
  | ContentBlockImage
  | ContentBlockAudio
  | ContentBlockVideo
  | ContentBlockFile


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
  /** spc_012-N-01: same additive contract as `ToolResultPart.contentParts` (see there). */
  contentParts?: ToolOutputBlock[]
}

export interface ToolSchema {
  name: string
  description: string
  parameters: string // JSON-encoded JSON Schema
  /** spc_001: vendor-specific extension bag, keyed by provider name. Preserved through
   *  normalization/lowering, never flattened into portable fields. */
  providerOptions?: Record<string, unknown>
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

export interface UsageEvent extends StreamEvent {
  type: "usage"
  /** Full prompt size + output (the authoritative prompt size for context accounting). */
  totalTokens: number
  /** Full prompt size: uncached input + cache reads + cache writes. */
  inputTokens?: number
  outputTokens?: number
  /** Prompt tokens served from cache this request (billed ~0.1x). Subset of inputTokens. */
  cacheReadInputTokens?: number
  /** Prompt tokens written to cache this request (billed ~1.25x). Subset of inputTokens. */
  cacheCreationInputTokens?: number
  /** I1: per-slot pro-rata attribution of `cacheReadInputTokens`. Estimated, not authoritative —
   *  Anthropic returns a single cache-read total, so the SDK divides it evenly across the slots
   *  that carried a `cache_control` breakpoint on the request. Missing when the provider doesn't
   *  honor `cache_control` (OpenAI-family auto-cache) or when no breakpoints were placed. */
  cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number }
  /** Canonical provider stop reason. `max_tokens` drives the kernel's output-cap recovery. */
  stopReason?: "end_turn" | "tool_use" | "max_tokens" | "stop_sequence" | "content_filter" | "other"
  /** Original provider spelling for diagnostics only. RuntimeRunner never forwards it to Kernel. */
  rawStopReason?: string
  /** Normalized postflight usage parsed from this same raw provider response. */
  providerUsage?: ProviderUsage
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
  /** Text projection when the chunk carries text. */
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
  /** spc_012-N-02: structured multimodal blocks when the tool returned non-text content
   *  (e.g. an MCP screenshot). `content` stays the text projection; see `ToolResultPart.contentParts`. */
  contentParts?: ToolOutputBlock[]
}

/** R3-1: a workflow node's agent called the `submit_workflow_nodes` tool. The runner intercepts it
 *  (it cannot apply to the child's own kernel — the workflow lives in the parent) and surfaces the
 *  requested nodes as this event; the `runWorkflow` driver sends them to the parent kernel. */
export interface WorkflowNodesSubmittedEvent extends StreamEvent {
  type: "workflow_nodes_submitted"
  nodes: WorkflowNodeSpec[]
}

export interface DoneEvent extends StreamEvent {
  type: "done"
  iterations: number
  totalTokens: number
  status: string
  /** ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs). */
  paceDecision?: import("./runtime/kernel-step.js").PaceDecision
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

/** Kernel session-entropy measurement at a completed turn boundary. "Entropy" = session
 *  disorder: repetition, tool failures, rollbacks, context pressure. The component vector is
 *  the contract; `score` is the canonical default fold. All normalized
 *  components are in [0, 1]. */
export interface EntropySample {
  turn: number
  score: number
  /** Context pressure after this boundary's eviction pass. */
  rho: number
  /** Consecutive-identical-turn streak, normalized against the RepeatFuse deny rung. */
  repeatPressure: number
  /** Errored tool results / total tool results over the sliding window. */
  failureRate: number
  /** Raw rollback count inside the window (normalize with `windowTurns`). */
  rollbacksInWindow: number
  /** Effective window size in completed turns. */
  windowTurns: number
}

/** One kernel entropy sample, emitted once per completed turn (a heartbeat watch source:
 *  subscribe to drive an external supervisor without tailing the audit log). */
export interface EntropySampleEvent extends StreamEvent {
  type: "entropy_sample"
  sample: EntropySample
}

/** The opt-in kernel entropy watch tripped: `score` crossed `threshold` while armed and
 *  cooled down (see `RunnerOptions.entropyWatch`). Correlate components via the same-turn
 *  `entropy_sample` event. */
export interface EntropyAlertEvent extends StreamEvent {
  type: "entropy_alert"
  turn: number
  score: number
  threshold: number
}

/** Opt-in kernel-side threshold watch over the per-turn entropy score. Sampling itself is
 *  unconditional; this only controls alerting. `notifyModel` additionally routes the alert
 *  into the model's own signal channel (durable `[SIGNAL]` directive at the next boundary) —
 *  leave it off when a host supervisor injects task-aware guidance itself. */
export interface EntropyWatchOptions {
  enabled?: boolean
  /** Alert when `score >= threshold` (kernel default 0.65). */
  threshold?: number
  /** Re-arm only after the score falls below `threshold - hysteresis` (default 0.1). */
  hysteresis?: number
  /** Minimum completed turns between two alerts (default 4). */
  cooldownTurns?: number
  notifyModel?: boolean
}

export interface TokenUsage {
  /** Full prompt size: uncached input + cache reads + cache writes. */
  inputTokens: number
  outputTokens: number
  totalTokens: number
  /** Prompt tokens served from cache (billed ~0.1x). Subset of inputTokens. */
  cacheReadInputTokens?: number
  /** Prompt tokens written to cache (billed ~1.25x). Subset of inputTokens. */
  cacheCreationInputTokens?: number
}

/** Raw postflight token facts normalized across provider wire shapes. */
export interface ProviderUsage {
  inputTokens: number
  outputTokens: number
  cacheReadInputTokens?: number
  cacheCreationInputTokens?: number
  /** Output tokens spent on hidden reasoning (OpenAI `completion_tokens_details.reasoning_tokens` /
   *  Responses `output_tokens_details.reasoning_tokens`). A SUBSET of `outputTokens`, not additional
   *  — vendors that don't report a separate count (Anthropic, Gemini via this SDK) leave this unset
   *  rather than guessing. */
  reasoningTokens?: number
}

/** Node-side mirror of the reserved Rust `context::measurement` types — where a
 *  preflight token count came from. Field names/shape intentionally match the Rust
 *  `MeasurementSource` enum (`kind`-tagged, snake_case variant names) so the two sides can be
 *  compared/round-tripped without a translation layer. A-00R removed the non-durable adaptive
 *  scheduler producer; the shape remains for the native provider meter implementations. */
export type MeasurementSource =
  | { kind: "native"; provider: string }
  | { kind: "local_exact"; tokenizer: string }
  | { kind: "heuristic" }

export type MeasurementConfidence = "exact" | "high_confidence" | "low_confidence"

/** A single preflight token-count fact for a candidate render, for a specific provider/model —
 *  the Node counterpart of Rust's `PromptMeasurement` (spc_011-C-02). */
export interface PromptMeasurement {
  inputTokens: number
  source: MeasurementSource
  confidence: MeasurementConfidence
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

export type ProviderProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "gemini"

/**
 * Strategy for placing Anthropic-protocol `cache_control` breakpoints across a request's
 * static prefix (tools + system blocks) and rolling history (messages). Pass via the
 * `extensions.cacheBreakpointStrategy` extension on every provider call; the runner already
 * flows `RuntimeOptions.extensions` through, so setting it once on the runner propagates
 * to every Anthropic-protocol call.
 *
 * Values:
 *   - `"default"` — current production behavior: a breakpoint on the last tool (when system
 *     is rendered as a string), one on each system block (when system blocks are present),
 *     and the rolling message pair (last message + frozen-prefix anchor or last preceding
 *     user turn).
 *   - `"tools-only"` — breakpoint on the last tool only. System blocks and history go
 *     uncached. Useful to isolate the tools-prefix cache contribution.
 *   - `"system-only"` — breakpoints on system blocks only. No tool, no history caching.
 *     Useful to isolate the system-prefix cache contribution.
 *   - `"frozen-prefix"` — breakpoints on the message history only, anchored at
 *     `frozenPrefixLen` (the compaction boundary, P1-E). Falls back to the last-message
 *     breakpoint when no frozen prefix is set. No tools, no system caching. Useful to
 *     stress-test the P1-E deep-anchor design.
 *   - `"none"` — no `cache_control` anywhere. The baseline for cache-hit attribution.
 *
 * Strategies that disable some breakpoints still keep the structural shape (e.g. system
 * blocks remain text blocks rather than collapsing into a single string), so the only
 * Δ between variants is which blocks carry `cache_control`. Unrecognised strings fall
 * back to `"default"`.
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

/** Provider-native fields required to replay a turn across requests (thinking blocks, reasoning_content, etc.). */
export interface ProviderReplay {
  provider?: string
  protocol: ProviderProtocol
  model?: string
  /** Anthropic-style assistant content blocks (thinking, text, tool_use). */
  native_blocks?: Array<Record<string, unknown>>
  /** OpenAI-compatible reasoning field (DeepSeek, etc.). */
  reasoning_content?: string
  reasoning_details?: unknown
  native_message?: unknown
  tool_calls?: unknown[]
}

/** Result of a pre-flight reasoning-replay assessment for a target provider. */
export interface ReplayabilityAssessment {
  /** True when every reasoning-requiring tool-call turn has replay available. */
  ok: boolean
  /** Tool-call ids whose turn lacks the required non-empty reasoning replay. */
  offendingCallIds: string[]
}

/** Structured render output produced by the kernel for each LLM call. */
export interface RenderedContext {
  /** Identity + Knowledge combined — for providers with a single system slot (OpenAI). */
  systemText: string
  /** Identity only (system partition). Anthropic system[0] with cache_control. */
  systemStable?: string
  /** Knowledge (memory retrievals, skill definitions, artifacts). Anthropic system[1] with cache_control. */
  systemKnowledge?: string
  /** History turns only — the stable, cacheable message prefix. */
  turns: Message[]
  /**
   * Volatile State turn (task_state + signals), rebuilt every call. Providers
   * render it after the cacheable history (Anthropic: after the cache breakpoint;
   * OpenAI-family: prepended, preserving order). Absent when produced by an
   * older binding that has not been rebuilt — then the State turn is still inside
   * `turns[0]` and providers render `turns` as-is.
   */
  stateTurn?: Message
  /**
   * P1-E: count of leading `turns` forming the frozen prefix — byte-stable until the next
   * compaction. The Anthropic provider pins a deep cache breakpoint at this boundary (a long-lived
   * cache that survives many turns and is immune to the 20-block lookback miss on heavy tool turns)
   * and rolls the other breakpoint at the tail. Absent (older binding, or no distinct frozen region
   * yet) ⇒ the provider falls back to the rolling-pair placement.
   */
  frozenPrefixLen?: number
  budgetOverflow?: ContextBudgetOverflow
}

export interface ContextBudgetOverflow {
  kind: "fixed_context" | "protected_tail"
  requiredTokens: number
  maxTokens: number
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
  descriptor?(): ProviderDescriptor
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
  /**
   * Pre-flight query: would this history validate against this provider with the
   * given extensions, without sending the request? Returns the tool-call ids
   * whose turn lacks the reasoning replay this provider requires, so an embedder
   * can route around the failure (keep thinking on, disable it, or skip this
   * candidate) before issuing the request. Seed any persisted replay first.
   */
  assessReplayability?(context: RenderedContext, extensions?: Record<string, unknown>): ReplayabilityAssessment
  /**
   * spc_011-C-02/03: preflight token count via the provider's own native counting endpoint
   * (Anthropic `messages.countTokens` and Gemini `countTokens`),
   * where the vendor offers one. Optional — providers without a native endpoint simply omit it,
   * and callers fall back to `FallbackEstimator` (Rust `context::token_engine`, spc_011-C-01) or
   * a local tokenizer. Not currently invoked by any dispatch loop (nothing in the Rust kernel
   * emits `EffectKind::MeasurePrompt` yet — see its doc comment); this method exists so the
   * capability remains directly callable, but no dispatch trigger is enabled until request
   * fingerprinting and durable measurement semantics are defined.
   */
  countTokens?(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<PromptMeasurement>
  complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message>
  stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
    /** #2-B-ii: when provided, a preempting `InterruptNow` (or `interrupt()`) aborts the in-flight
     *  request. SDK-client providers should forward it to the client (`{ signal }`); the runner also
     *  breaks the consume loop on abort, so providers that ignore it still stop processing immediately
     *  (only the socket lingers). */
    signal?: AbortSignal,
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
 * Durable-memory summarizer for semantic `page_out` events (Layer 5 contract).
 * The kernel emits `page_out { tier_hint: "semantic" }`; the SDK persists an LLM summary to MemoryStore.
 */
export interface MemorySummarizer {
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
