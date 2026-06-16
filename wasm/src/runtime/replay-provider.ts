/**
 * ReplayProvider — an LLMProvider that emits previously-recorded assistant messages
 * instead of calling a real LLM API.
 *
 * Purpose: deterministic re-runs for benchmarking, CI, and golden regression. Useful when you want
 * to hold the model's behavior constant and measure something else (prompt-size cost Δ across
 * RuntimeOptions variants, codegen of follow-on kernel work, etc.).
 *
 * Distinct from `provider-replay.ts`: that file's `seedProviderReplay` / `peekProviderReplay` is a
 * session-repair *reasoning-content cache* — it preserves `reasoning_content` / `native_blocks` so
 * the model sees its own thinking when context is re-rendered. It does NOT skip LLM calls.
 * `ReplayProvider` is the orthogonal, request-skipping mechanism: it returns recorded responses
 * directly, never hitting an API.
 *
 * Cost-accounting under replay:
 *   - `inputTokens` is ESTIMATED from the rendered context this call carries (NOT a recorded value
 *     from the original run). That's the point of replay-for-benchmarking: prompt may differ across
 *     variants, response is pinned, so a cost Δ purely reflects the prompt change.
 *   - `outputTokens` is taken from `message.tokenCount` when present; otherwise estimated from
 *     `message.content.length / 4`.
 *   - `cacheReadInputTokens` / `cacheCreationInputTokens` are emitted as 0 — replay has no real
 *     cache state. Mechanisms whose Δ depends on cache behavior must validate with a live A/B too.
 *
 * Tokenizer: by default a `chars/4` estimator (±20% for English; worse for code/JSON). For tighter
 * numbers plug `opts.tokenizer = tiktokenEncoder` or similar.
 */

import type {
  LLMProvider,
  Message,
  ProviderDescriptor,
  ProviderRunState,
  RenderedContext,
  StreamEvent,
  TextDelta,
  ToolCallEvent,
  ToolSchema,
  UsageEvent,
} from "../types.js"

export interface ReplayProviderOpts {
  /**
   * Maps a rendered-context text payload to a token count. Defaults to `chars / 4`.
   * Pass a real encoder (tiktoken etc.) for accurate cost accounting under replay.
   */
  tokenizer?: (text: string) => number
  /**
   * Provider descriptor advertised via `descriptor()`. Defaults to a generic
   * `{ provider: "replay", protocol: "replay", ... }` shape. Override when a downstream consumer
   * needs to detect the original provider (e.g., for protocol-specific decoding paths).
   */
  descriptor?: ProviderDescriptor
  /**
   * When true, `stream()` and `complete()` wrap to the start once the fixture is exhausted,
   * instead of throwing. Useful for loop tests that need to keep going past the recorded length.
   * Defaults to false.
   */
  wrap?: boolean
}

const DEFAULT_DESCRIPTOR: ProviderDescriptor = {
  provider: "replay",
  protocol: "openai-chat",
  model: "replay",
  reasoning: { supported: false, preserveAcrossToolTurns: false },
  toolCalls: { supported: true, requiresStrictPairing: false },
}

export class ReplayProvider implements LLMProvider {
  private cursor = 0
  private readonly messages: ReadonlyArray<Message>
  private readonly tokenizer: (text: string) => number
  private readonly _descriptor: ProviderDescriptor
  private readonly wrap: boolean

  /**
   * @param messages Ordered list of assistant messages to replay (one per LLM call).
   * @param opts Optional tokenizer / descriptor / wrap-around behavior.
   */
  constructor(messages: ReadonlyArray<Message>, opts: ReplayProviderOpts = {}) {
    this.messages = messages
    this.tokenizer = opts.tokenizer ?? defaultTokenizer
    this._descriptor = opts.descriptor ?? DEFAULT_DESCRIPTOR
    this.wrap = !!opts.wrap
  }

  descriptor(): ProviderDescriptor {
    return this._descriptor
  }

  /** Number of messages consumed so far. */
  consumed(): number {
    return this.cursor
  }

  /** Number of messages remaining in the fixture (returns 0 in wrap mode once cursor passes end). */
  remaining(): number {
    return Math.max(0, this.messages.length - this.cursor)
  }

  /** Reset the cursor — useful for re-running the same fixture in a fresh session. */
  reset(): void {
    this.cursor = 0
  }

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    const msg = this.pull()
    return {
      role: "assistant",
      content: msg.content,
      ...(msg.toolCalls ? { toolCalls: msg.toolCalls } : {}),
      ...(msg.tokenCount !== undefined ? { tokenCount: msg.tokenCount } : {}),
    }
  }

  async *stream(
    context: RenderedContext,
    tools: ToolSchema[],
    _extensions?: Record<string, unknown>,
    _state?: ProviderRunState,
    _signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const msg = this.pull()
    const inputTokens = this.estimateInputTokens(context, tools)
    const outputTokens =
      msg.tokenCount !== undefined ? msg.tokenCount : this.tokenizer(msg.content || "")

    const usage: UsageEvent = {
      type: "usage",
      totalTokens: inputTokens + outputTokens,
      inputTokens,
      outputTokens,
      cacheReadInputTokens: 0,
      cacheCreationInputTokens: 0,
    }
    yield usage

    if (msg.content) {
      const delta: TextDelta = { type: "text_delta", delta: msg.content }
      yield delta
    }

    for (const tc of msg.toolCalls ?? []) {
      let args: Record<string, unknown> = {}
      try {
        args = JSON.parse(tc.arguments || "{}")
      } catch {
        // Malformed recorded arguments — pass an empty object. The runner's downstream tool
        // execution will surface the error if the tool needs them.
      }
      const call: ToolCallEvent = { type: "tool_call", id: tc.id, name: tc.name, arguments: args }
      yield call
    }
  }

  private pull(): Message {
    if (this.cursor >= this.messages.length) {
      if (this.wrap && this.messages.length > 0) {
        this.cursor = 0
      } else {
        throw new Error(
          `ReplayProvider: fixture exhausted (consumed=${this.cursor}, total=${this.messages.length})`,
        )
      }
    }
    return this.messages[this.cursor++]
  }

  private estimateInputTokens(context: RenderedContext, tools: ToolSchema[]): number {
    const text = renderContextToText(context, tools)
    return this.tokenizer(text)
  }
}

// ── helpers ───────────────────────────────────────────────────────────────────

function defaultTokenizer(text: string): number {
  return Math.ceil(text.length / 4)
}

function renderContextToText(context: RenderedContext, tools: ToolSchema[]): string {
  const parts: string[] = []
  if (context.systemText) parts.push(context.systemText)
  if (context.systemStable) parts.push(context.systemStable)
  if (context.systemKnowledge) parts.push(context.systemKnowledge)
  if (context.stateTurn?.content) parts.push(context.stateTurn.content)
  for (const turn of context.turns ?? []) {
    if (turn.content) parts.push(turn.content)
    for (const part of turn.contentParts ?? []) {
      const p = part as unknown as Record<string, unknown>
      if (typeof p.output === "string") parts.push(p.output)
      else if (typeof p.text === "string") parts.push(p.text)
    }
    for (const tc of turn.toolCalls ?? []) {
      parts.push(`${tc.name} ${tc.arguments}`)
    }
  }
  for (const tool of tools) {
    parts.push(`${tool.name} ${tool.description} ${tool.parameters}`)
  }
  return parts.join("\n")
}
