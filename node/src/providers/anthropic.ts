import { requestSnapshot } from "./prepared-request.js"
import type { PreparedProviderRequest, ProviderRunState as PreparedRunState } from "../types.js"
import Anthropic from "@anthropic-ai/sdk"
import type {
  CacheBreakpointStrategy,
  LLMProvider,
  ModelMessage,
  NativeContextTiers,
  PromptMeasurement,
  ProviderDescriptor,
  ProviderReplay,
  ProviderRunState,
  ProviderTransportTelemetry,
  RenderedContext,
  RuntimePolicy,
  StreamEvent,
  ToolSchema,
} from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker } from "./base.js"
import {
  AnthropicMessagesAdapter,
  type AnthropicRequestPlan,
  type AnthropicStreamChunk,
} from "./anthropic-adapter.js"
import {
  normalizeCanonicalAdapterInput,
  type CanonicalAdapterInput,
} from "./content-normalization.js"
import { endpointProfiles, type ProviderId } from "./endpoints.js"
import {
  OnDemandCompactor,
  THRESHOLD_COMPACTION_BETA,
  compactionBlock,
  resolveCompaction,
  resumeAfterCompactionPause,
  withoutCompactionPause,
  type AnthropicCompaction,
} from "./anthropic-compaction.js"

export type {
  AnthropicCompaction,
  AnthropicOnDemandCompaction,
  AnthropicThresholdCompaction,
} from "./anthropic-compaction.js"
import { circuitOpenError, classifyProviderError } from "./provider-error.js"

export interface AnthropicProviderConfig {
  apiKey: string
  model?: string
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
  authMode?: "api-key" | "bearer"
  runtimePolicy?: RuntimePolicy
  /** Clear old tool results server-side (context editing, `clear_tool_uses_20250919`). The
   *  provider then declares the tier native and the kernel stops clearing tool results itself;
   *  `true` takes the API defaults. */
  contextEditing?: true | AnthropicContextEditing
  /** Summarize earlier history server-side: at a threshold on every request (`compact_20260112`,
   *  the default mode), or on demand once a turn passes a limit (`compact-2026-09-04`, `mode:
   *  "on_demand"`). The provider then declares compaction native and the kernel stops compacting
   *  itself; its renewal stays the safety net, so set the trigger below the kernel's renewal
   *  threshold. `true` takes the threshold API defaults. */
  compaction?: true | AnthropicCompaction
}

/** `clear_tool_uses_20250919` settings. Unset fields take the API defaults. */
export interface AnthropicContextEditing {
  /** Input tokens above which clearing starts. */
  triggerInputTokens?: number
  /** Most recent tool uses kept intact. */
  keepToolUses?: number
  /** Skip a clearing that would remove fewer tokens than this — each clearing rewrites the cached prefix. */
  clearAtLeastInputTokens?: number
  /** Tools whose uses and results are never cleared. */
  excludeTools?: string[]
  /** Also clear the tool-call inputs, not only the results. */
  clearToolInputs?: boolean
}

const CONTEXT_MANAGEMENT_BETA = "context-management-2025-06-27"

/** One server-side context edit and the beta that enables it. */
interface NativeContextEdit {
  beta: string
  edit: Record<string, unknown>
}

function clearToolUsesEdit(config: true | AnthropicContextEditing): Record<string, unknown> {
  const c = config === true ? {} : config
  return {
    type: "clear_tool_uses_20250919",
    ...(c.triggerInputTokens !== undefined ? { trigger: { type: "input_tokens", value: c.triggerInputTokens } } : {}),
    ...(c.keepToolUses !== undefined ? { keep: { type: "tool_uses", value: c.keepToolUses } } : {}),
    ...(c.clearAtLeastInputTokens !== undefined ? { clear_at_least: { type: "input_tokens", value: c.clearAtLeastInputTokens } } : {}),
    ...(c.excludeTools?.length ? { exclude_tools: [...c.excludeTools] } : {}),
    ...(c.clearToolInputs !== undefined ? { clear_tool_inputs: c.clearToolInputs } : {}),
  }
}

type ResolvedAnthropicRuntime = CanonicalAdapterInput["resolved"]

export class AnthropicProvider implements LLMProvider {
  private client: Anthropic
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number
  protected readonly model: string
  private readonly adapter = new AnthropicMessagesAdapter()
  private readonly nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()
  private readonly resolvedRuntimePolicy: RuntimePolicy
  private readonly directNativeTokenCounting: boolean
  private readonly defaultTextualToolCallPolicy: "off" | "reject"
  private readonly nativeContextEdits: NativeContextEdit[] = []
  /** Threshold compaction with a pause: how many history messages stay verbatim behind the block. */
  private readonly compactionPauseKeep?: number
  private readonly onDemandCompactor?: OnDemandCompactor
  private resolvedRuntime?: ResolvedAnthropicRuntime

  constructor(config: AnthropicProviderConfig) {
    if (!config || typeof config !== "object" || Array.isArray(config)) {
      throw new TypeError("AnthropicProvider requires a configuration object")
    }
    if (typeof config.apiKey !== "string" || config.apiKey.length === 0) {
      throw new TypeError("AnthropicProvider requires a non-empty apiKey")
    }
    const c: AnthropicProviderConfig = {
      model: "claude-sonnet-4-6",
      retry: { maxRetries: 3, baseDelay: 1000 },
      ...config,
    }
    this.model = c.model ?? "claude-sonnet-4-6"
    this.client = withServerRuntimeGuard(() => new Anthropic({
      ...(c.authMode === "bearer"
        ? { authToken: c.apiKey, apiKey: null as unknown as string }
        : { apiKey: c.apiKey, authToken: null as unknown as string }),
      ...(c.baseURL ? { baseURL: c.baseURL } : {}),
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = c.retry?.maxRetries ?? 3
    this.baseDelay = c.retry?.baseDelay ?? 1000
    this.resolvedRuntimePolicy = c.runtimePolicy ?? {}
    const configuredBaseURL = c.baseURL?.replace(/\/+$/, "")
    const officialBaseURL = endpointProfiles["anthropic.messages"].baseURL.replace(/\/+$/, "")
    this.directNativeTokenCounting = configuredBaseURL === undefined
      || configuredBaseURL === officialBaseURL
    this.defaultTextualToolCallPolicy = this.directNativeTokenCounting ? "off" : "reject"
    // Clearing runs first so the compaction trigger reads the input after tool results are cleared.
    if (c.contextEditing) this.nativeContextEdits.push({ beta: CONTEXT_MANAGEMENT_BETA, edit: clearToolUsesEdit(c.contextEditing) })
    const compaction = c.compaction ? resolveCompaction(c.compaction) : undefined
    if (compaction?.mode === "threshold") {
      this.nativeContextEdits.push({ beta: THRESHOLD_COMPACTION_BETA, edit: compaction.edit })
      if (compaction.edit.pause_after_compaction) this.compactionPauseKeep = compaction.keepRecentMessages ?? 0
    } else if (compaction?.mode === "on_demand") {
      this.onDemandCompactor = new OnDemandCompactor(
        compaction.config,
        body => this.createMessage(body, "beta"),
      )
    }
  }

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  /** Identity advertised in the descriptor; overridden by Anthropic-compatible vendors. */
  protected providerName(): string {
    return "anthropic"
  }

  descriptor(): ProviderDescriptor {
    return {
      provider: this.providerName(),
      protocol: "anthropic-messages",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: true,
        requiresReplayForToolTurns: true,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
      ...(this.nativeContextEdits.length || this.onDemandCompactor ? { nativeContext: this.nativeContextTiers() } : {}),
    }
  }

  /** P4-S1: transport facts of the most recent execution (rung count + wire response id).
   *  Host evidence only — never kernel input (B7). */
  private lastTelemetry: ProviderTransportTelemetry | undefined

  peekTransportTelemetry(): ProviderTransportTelemetry | undefined {
    return this.lastTelemetry
  }

  bindResolvedRuntime(resolved: ResolvedAnthropicRuntime): void {
    if (
      resolved.identity.protocol !== "anthropic-messages"
      || resolved.identity.providerId !== this.providerName()
      || resolved.identity.modelId !== this.model
    ) {
      throw new Error("AnthropicProvider received a mismatched resolved runtime")
    }
    this.resolvedRuntime = resolved
  }

  peekProviderReplay(message: Pick<ModelMessage, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { protocol: "anthropic-messages", native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<ModelMessage, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.protocol === "anthropic-messages" && replay.native_blocks?.length) {
      this.nativeAssistantBlocks.set(assistantReplayKey(message), replay.native_blocks)
    }
  }

  private adapterInput(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): CanonicalAdapterInput {
    const providerId = this.providerName() as ProviderId
    const endpoint = Object.values(endpointProfiles).find(profile =>
      profile.providerId === providerId && profile.protocol === "anthropic-messages",
    ) ?? endpointProfiles["anthropic.messages"]
    const resolved = this.resolvedRuntime ?? {
      identity: {
        providerId,
        modelId: this.model,
        endpointId: endpoint.id,
        protocol: "anthropic-messages",
      },
      model: {
        id: `${providerId}/${this.model}`,
        providerId,
        kind: "generation",
        intrinsic: {},
      },
      endpoint,
      adapter: this,
      effectiveCapabilities: compatibilityCapabilities(),
    } as unknown as ResolvedAnthropicRuntime
    return normalizeCanonicalAdapterInput({
      context,
      tools,
      resolved,
      extensions: {
        textualToolCallPolicy: this.defaultTextualToolCallPolicy,
        ...extensions,
        ...this.nativeContextExtensions(extensions),
      },
      replayForMessage: message => this.peekProviderReplay(message),
    })
  }

  private nativeContextTiers(): NativeContextTiers {
    const types = new Set(this.nativeContextEdits.map(native => native.edit.type))
    return {
      ...(types.has("clear_tool_uses_20250919") ? { toolResultClearing: true } : {}),
      ...(types.has("compact_20260112") || this.onDemandCompactor ? { compaction: true } : {}),
    }
  }

  /** The native edits ride on every request, beside any betas or edits the caller passed: the
   *  kernel stopped running those tiers on the strength of them. */
  private nativeContextExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    if (!this.nativeContextEdits.length) return {}
    const betas = Array.isArray(extensions?.betas) ? [...extensions.betas as string[]] : []
    const management = extensions?.context_management as { edits?: unknown[] } | undefined
    const edits = Array.isArray(management?.edits) ? [...management.edits] : []
    for (const native of this.nativeContextEdits) {
      if (!betas.includes(native.beta)) betas.push(native.beta)
      if (!edits.some(edit => (edit as { type?: unknown })?.type === native.edit.type)) edits.push(native.edit)
    }
    return { betas, context_management: { ...management, edits } }
  }

  private buildPlan(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): { input: CanonicalAdapterInput; plan: AnthropicRequestPlan } {
    const input = this.adapterInput(context, tools, extensions)
    const plan = this.adapter.buildRequest(input)
    if (this.onDemandCompactor?.apply(plan.params)) plan.transport = "beta"
    return { input, plan }
  }

  /** On-demand compaction in flight settles; for tests and orderly shutdown. */
  compactionIdle(): Promise<void> {
    return this.onDemandCompactor?.idle() ?? Promise.resolve()
  }

  /** The continuation of a paused threshold compaction. When recent turns are kept, the block now
   *  heads an earlier assistant turn, and that turn's replay is rewritten so every later request
   *  renders the same messages the continuation sent. */
  private resumeCompactionPause(
    context: RenderedContext,
    plan: AnthropicRequestPlan,
    block: Record<string, unknown>,
  ): { params: Record<string, unknown>; anchored: boolean } {
    const resumed = resumeAfterCompactionPause(
      plan.params.messages as Array<Record<string, unknown>>,
      historyCount(plan),
      block,
      this.compactionPauseKeep,
    )
    if (resumed.anchor) {
      const turn = context.turns[plan.wireTurns[resumed.anchor.wireIndex]]
      if (turn) this.nativeAssistantBlocks.set(assistantReplayKey(turn), resumed.anchor.content)
    }
    return { params: { ...withoutCompactionPause(plan.params), messages: resumed.messages }, anchored: !!resumed.anchor }
  }

  async complete(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): Promise<ModelMessage> {
    const provider = this.providerName()
    if (this.circuit.isOpen()) throw circuitOpenError(provider)
    let input: CanonicalAdapterInput
    let plan: AnthropicRequestPlan
    try {
      ;({ input, plan } = this.buildPlan(context, tools, extensions))
    } catch (error) {
      throw classifyProviderError(provider, error)
    }

    let lastErr: unknown
    for (let attempt = 0; attempt < this.maxRetries; attempt++) {
      try {
        this.lastTelemetry = { rungs: attempt + 1 }
        let raw = await this.createMessage(plan.params, plan.transport)
        const paused = this.compactionPauseKeep !== undefined && raw.stop_reason === "compaction"
          ? compactionBlock(raw.content)
          : undefined
        let unanchoredBlock: Record<string, unknown> | undefined
        if (paused) {
          const resumed = this.resumeCompactionPause(context, plan, paused)
          if (!resumed.anchored) unanchoredBlock = paused
          raw = await this.createMessage(resumed.params, "beta")
        }
        this.circuit.recordSuccess()
        if (typeof raw.id === "string") this.lastTelemetry = { rungs: attempt + 1, responseId: raw.id }
        const decoded = this.adapter.decodeComplete(raw, { input })
        const replay = [...(unanchoredBlock ? [unanchoredBlock] : []), ...(decoded.replay?.native_blocks ?? [])]
        if (replay.length) this.rememberNativeBlocks(decoded.message, replay)
        const usage = this.adapter.normalizeUsage(raw.usage)
        if (usage) this.onDemandCompactor?.observe(plan.params, historyCount(plan), usage.inputTokens + usage.outputTokens)
        return decoded.message
      } catch (error) {
        lastErr = error
        this.circuit.recordFailure()
        if (attempt < this.maxRetries - 1) {
          await new Promise(resolve => setTimeout(resolve, this.baseDelay * 2 ** attempt))
        }
      }
    }
    throw classifyProviderError(provider, lastErr)
  }

  /** Native measurement belongs to the verified official endpoint, not the wire protocol. */
  async countTokens(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): Promise<PromptMeasurement> {
    return this.countPlan(this.buildPlan(context, tools, extensions).plan)
  }

  private async countPlan(plan: AnthropicRequestPlan): Promise<PromptMeasurement> {
    const enabled = this.resolvedRuntime
      ? this.resolvedRuntime.effectiveCapabilities.nativeTokenCounting.state === "supported"
      : this.providerName() === "anthropic" && this.directNativeTokenCounting
    if (!enabled) {
      throw new Error(`Native token counting is unavailable on ${this.providerName()} Anthropic-compatible endpoint`)
    }
    const { model, system, messages, tools: requestTools, betas } = plan.params
    const body = requestSnapshot({
      model,
      ...(system ? { system } : {}),
      messages,
      ...(requestTools ? { tools: requestTools } : {}),
    })
    // A history that carries a compaction block is only readable under its beta.
    const response = plan.transport === "beta" && Array.isArray(betas)
      ? await this.client.beta.messages.countTokens({ ...body, betas } as unknown as Parameters<typeof this.client.beta.messages.countTokens>[0])
      : await this.client.messages.countTokens(body as unknown as Anthropic.MessageCountTokensParams)
    return {
      inputTokens: response.input_tokens,
      source: { kind: "native", provider: "anthropic" },
      confidence: "exact",
    }
  }

  prepareRequest(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: PreparedRunState): PreparedProviderRequest {
    const { input, plan: built } = this.buildPlan(context, tools, extensions)
    const plan = requestSnapshot(built)
    plan.params.stream = true
    return {
      scope: "encoded_body", request: requestSnapshot({ body: plan.params, transport: plan.transport }), state: requestSnapshot(state ?? null),
      stream: signal => this.streamPlan(context, input, plan, signal),
      countTokens: () => this.countPlan(plan),
    }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: PreparedRunState, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    yield* this.prepareRequest(context, tools, extensions, state).stream(signal)
  }

  private async *streamPlan(
    context: RenderedContext,
    input: CanonicalAdapterInput,
    plan: AnthropicRequestPlan,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const provider = this.providerName()
    try {
      let state = this.adapter.createStreamState({ input })
      this.lastTelemetry = { rungs: 1 }
      // With a compaction pause configured, a response that opens with a compaction block is held
      // back: if it stops there, it is the pause, and the continuation's events are the turn's.
      let held: StreamEvent[] | undefined = this.compactionPauseKeep !== undefined ? [] : undefined
      let paused = false
      for await (const chunk of this.streamMessage(plan.params, plan.transport, signal)) {
        if (this.lastTelemetry.responseId === undefined && chunk.type === "message_start") {
          const id = (chunk as Record<string, any>).message?.id
          if (typeof id === "string") this.lastTelemetry = { rungs: 1, responseId: id }
        }
        const events = this.adapter.pushStreamChunkAtBoundary({ chunk, state }).events
        if (held && chunk.type === "content_block_start" && chunk.content_block?.type !== "compaction") {
          yield* held
          held = undefined
        }
        if (held && chunk.type === "message_delta" && chunk.delta?.stop_reason === "compaction") paused = true
        if (held) held.push(...events)
        else yield* events
      }
      const pausedBlock = paused
        ? Object.values(state.nativeBlocks).find(block => block.type === "compaction")
        : undefined
      let unanchoredBlock: Record<string, unknown> | undefined
      if (pausedBlock) {
        const resumed = this.resumeCompactionPause(context, plan, pausedBlock)
        if (!resumed.anchored) unanchoredBlock = pausedBlock
        state = this.adapter.createStreamState({ input })
        for await (const chunk of this.streamMessage(resumed.params, "beta", signal)) {
          yield* this.adapter.pushStreamChunkAtBoundary({ chunk, state }).events
        }
      } else if (held) {
        yield* held
      }
      const final = this.adapter.finishStreamAtBoundary({ state })
      yield* final.events
      const replay = [...(unanchoredBlock ? [unanchoredBlock] : []), ...(final.replay?.native_blocks ?? [])]
      if (replay.length) {
        this.rememberNativeBlocks({ content: state.finalText, toolCalls: state.finalToolCalls }, replay)
      }
      this.onDemandCompactor?.observe(
        plan.params,
        historyCount(plan),
        state.uncachedInput + state.cacheReadTokens + state.cacheCreationTokens + state.outputTokens,
      )
    } catch (error) {
      throw classifyProviderError(provider, error)
    }
  }

  private createMessage(
    params: Record<string, unknown>,
    transport: AnthropicRequestPlan["transport"],
  ): Promise<Record<string, unknown>> {
    return (transport === "beta"
      ? this.client.beta.messages.create(
        params as unknown as Parameters<typeof this.client.beta.messages.create>[0],
      )
      : this.client.messages.create(params as unknown as Anthropic.MessageCreateParamsNonStreaming)
    ) as unknown as Promise<Record<string, unknown>>
  }

  private streamMessage(
    params: Record<string, unknown>,
    transport: AnthropicRequestPlan["transport"],
    signal?: AbortSignal,
  ): AsyncIterable<AnthropicStreamChunk> {
    const options = signal ? { signal } : undefined
    return (transport === "beta"
      ? this.client.beta.messages.stream(
        params as unknown as Parameters<typeof this.client.beta.messages.stream>[0],
        options,
      )
      : this.client.messages.stream(
        params as unknown as Anthropic.MessageStreamParams,
        options,
      )
    ) as unknown as AsyncIterable<AnthropicStreamChunk>
  }

  // White-box test seams. Request construction itself belongs to the adapter.
  private buildSystem(context: RenderedContext, strategy: CacheBreakpointStrategy): unknown {
    return this.buildPlan(context, [], { cacheBreakpointStrategy: strategy }).plan.params.system
  }

  private buildMessages(context: RenderedContext, strategy: CacheBreakpointStrategy): unknown {
    return this.buildPlan(context, [], { cacheBreakpointStrategy: strategy }).plan.params.messages
  }

  private rememberNativeBlocks(
    message: Pick<ModelMessage, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!blocks.length) return
    if (!message.toolCalls?.length && !blocks.some(block => block.type === "thinking" || block.type === "compaction")) return
    this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }
}

/** The leading wire messages that are history; the state turn and placeholder follow them. */
function historyCount(plan: AnthropicRequestPlan): number {
  return plan.wireTurns.filter(index => index >= 0).length
}

function compatibilityCapabilities(): ResolvedAnthropicRuntime["effectiveCapabilities"] {
  const unknown = { state: "unknown" as const, evidence: [] }
  const unsupported = { state: "unsupported" as const, evidence: ["protocol" as const] }
  return {
    inputModalities: {
      text: unknown,
      image: unknown,
      audio: unsupported,
      video: unsupported,
      file: unsupported,
    },
    outputModalities: {
      text: unknown,
      image: unsupported,
      audio: unsupported,
      embedding: unsupported,
    },
    tools: unknown,
    reasoning: unknown,
    parallelToolCalls: unknown,
    structuredOutput: unsupported,
    promptCaching: unknown,
    nativeTokenCounting: unknown,
    mediaForms: {
      imageUrl: unknown,
      imageBase64: unknown,
      fileId: unsupported,
      audioUrl: unsupported,
      audioBase64: unsupported,
    },
  }
}
