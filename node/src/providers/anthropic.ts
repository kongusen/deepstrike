import Anthropic from "@anthropic-ai/sdk"
import type {
  CacheBreakpointStrategy,
  LLMProvider,
  Message,
  PromptMeasurement,
  ProviderDescriptor,
  ProviderReplay,
  ProviderRunState,
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
import { circuitOpenError, classifyProviderError } from "./provider-error.js"

export interface AnthropicProviderConfig {
  apiKey: string
  model?: string
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
  authMode?: "api-key" | "bearer"
  runtimePolicy?: RuntimePolicy
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
    this.directNativeTokenCounting = c.baseURL === undefined
      || c.baseURL === endpointProfiles["anthropic.messages"].baseURL
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
    }
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

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { protocol: "anthropic-messages", native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
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
      extensions,
      replayForMessage: message => this.peekProviderReplay(message),
    })
  }

  private buildPlan(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): { input: CanonicalAdapterInput; plan: AnthropicRequestPlan } {
    const input = this.adapterInput(context, tools, extensions)
    return { input, plan: this.adapter.buildRequest(input) }
  }

  async complete(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): Promise<Message> {
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
        const raw = await this.createMessage(plan.params, plan.transport)
        this.circuit.recordSuccess()
        const decoded = this.adapter.decodeComplete(raw, { input })
        if (decoded.replay?.native_blocks) {
          this.rememberNativeBlocks(decoded.message, decoded.replay.native_blocks)
        }
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
    const enabled = this.resolvedRuntime
      ? this.resolvedRuntime.effectiveCapabilities.nativeTokenCounting.state === "supported"
      : this.providerName() === "anthropic" && this.directNativeTokenCounting
    if (!enabled) {
      throw new Error(`Native token counting is unavailable on ${this.providerName()} Anthropic-compatible endpoint`)
    }
    const { plan } = this.buildPlan(context, tools, extensions)
    const { model, system, messages, tools: requestTools } = plan.params
    const response = await this.client.messages.countTokens({
      model,
      ...(system ? { system } : {}),
      messages,
      ...(requestTools ? { tools: requestTools } : {}),
    } as unknown as Anthropic.MessageCountTokensParams)
    return {
      inputTokens: response.input_tokens,
      source: { kind: "native", provider: "anthropic" },
      confidence: "exact",
    }
  }

  async *stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    _state?: ProviderRunState,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const provider = this.providerName()
    try {
      const { input, plan } = this.buildPlan(context, tools, extensions)
      const state = this.adapter.createStreamState({ input })
      for await (const chunk of this.streamMessage(plan.params, plan.transport, signal)) {
        for (const event of this.adapter.pushStreamChunk(chunk, state).events) yield event
      }
      const final = this.adapter.finishStream(state)
      for (const event of final.events) yield event
      if (final.replay?.native_blocks) {
        this.rememberNativeBlocks(
          { content: state.finalText, toolCalls: state.finalToolCalls },
          final.replay.native_blocks,
        )
      }
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
    message: Pick<Message, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!blocks.length) return
    if (!message.toolCalls?.length && !blocks.some(block => block.type === "thinking")) return
    this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }
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
