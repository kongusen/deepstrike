import OpenAI from "openai"
import type {
  LLMProvider,
  Message,
  ProviderDescriptor,
  ProviderReplay,
  ProviderRunState,
  RenderedContext,
  ReplayabilityAssessment,
  RuntimePolicy,
  StreamEvent,
  ToolSchema,
} from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys, stablePromptCacheKey } from "./base.js"
import {
  normalizeCanonicalAdapterInput,
  type CanonicalAdapterInput,
} from "./content-normalization.js"
import { endpointProfiles } from "./endpoints.js"
import {
  OpenAIChatAdapter,
  type OpenAIChatStreamChunk,
} from "./openai-chat.js"
import {
  openAIChatDialects,
  type OpenAIChatTurnReasoning,
  type OpenAIChatWireDialect,
} from "./openai-chat-dialects.js"
import { circuitOpenError, classifyProviderError } from "./provider-error.js"

/** Options-object form for `OpenAIProvider`. */
export interface OpenAIProviderOptions {
  apiKey: string
  model?: string
  retry?: { maxRetries: number; baseDelay: number }
  baseURL?: string
}

export type { OpenAIChatTurnReasoning } from "./openai-chat-dialects.js"

/** Compatibility helper retained for callers that rebuild native streamed tool blocks. */
export function nativeToolCallsFromBuffers(
  toolCallBuffers: Record<number, { id: string; name: string; argsBuf: string }>,
): Array<Record<string, unknown>> {
  return Object.values(toolCallBuffers).map(call => ({
    id: call.id,
    type: "function",
    function: { name: call.name, arguments: call.argsBuf || "{}" },
  }))
}

type ResolvedOpenAIChatRuntime = CanonicalAdapterInput["resolved"]

export class OpenAIChatProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly model: string
  protected readonly chat = new OpenAIChatAdapter()
  protected readonly dialect: OpenAIChatWireDialect
  private readonly replayStore = new Map<string, ProviderReplay>()
  private readonly resolvedRuntimePolicy: RuntimePolicy
  private resolvedRuntime?: ResolvedOpenAIChatRuntime

  constructor(
    apiKeyOrOptions: string | OpenAIProviderOptions,
    model = "gpt-4o",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL: string = endpointProfiles["openai.chat"].baseURL,
    runtimePolicy: RuntimePolicy = {},
    dialect: OpenAIChatWireDialect = openAIChatDialects.openai,
  ) {
    const options: Required<OpenAIProviderOptions> = typeof apiKeyOrOptions === "string"
      ? { apiKey: apiKeyOrOptions, model, retry, baseURL }
      : {
          model: "gpt-4o",
          retry: { maxRetries: 3, baseDelay: 1000 },
          baseURL: endpointProfiles["openai.chat"].baseURL,
          ...apiKeyOrOptions,
        }
    this.model = options.model
    this.client = withServerRuntimeGuard(() => new OpenAI({
      apiKey: options.apiKey,
      baseURL: options.baseURL,
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = options.retry.maxRetries
    this.baseDelay = options.retry.baseDelay
    this.resolvedRuntimePolicy = runtimePolicy
    this.dialect = dialect
  }

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  descriptor(): ProviderDescriptor {
    return {
      provider: this.dialect.providerId,
      protocol: "openai-chat",
      model: this.model,
      reasoning: this.dialect.descriptor.reasoning,
      toolCalls: { supported: true, requiresStrictPairing: true },
    }
  }

  bindResolvedRuntime(resolved: ResolvedOpenAIChatRuntime): void {
    if (
      resolved.identity.protocol !== "openai-chat"
      || resolved.identity.providerId !== this.dialect.providerId
      || resolved.identity.modelId !== this.model
    ) {
      throw new Error("OpenAIChatProvider received a mismatched resolved runtime")
    }
    this.resolvedRuntime = resolved
  }

  assessReplayability(
    context: RenderedContext,
    extensions?: Record<string, unknown>,
  ): ReplayabilityAssessment {
    const prepared = this.dialect.prepareExtensions(extensions ?? {})
    if (!this.dialect.requireReasoningReplay(prepared)) {
      return { ok: true, offendingCallIds: [] }
    }
    const offendingCallIds = context.turns.flatMap(message => {
      if (message.role !== "assistant" || !message.toolCalls?.length) return []
      const replay = this.peekProviderReplay(message)
      return typeof replay?.reasoning_content === "string" && replay.reasoning_content.trim()
        ? []
        : message.toolCalls.map(call => call.id)
    })
    return { ok: offendingCallIds.length === 0, offendingCallIds }
  }

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const replay = this.replayStore.get(assistantReplayKey(message))
    if (!replay || !("reasoning_content" in replay || "reasoning_details" in replay)) return undefined
    if (this.dialect.id === "qwen" && replay.reasoning_content !== undefined) {
      return { reasoning_content: String(replay.reasoning_content ?? "") }
    }
    return replay
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.reasoning_content === undefined && replay.reasoning_details === undefined) return
    this.replayStore.set(assistantReplayKey(message), this.dialect.id === "qwen"
      ? { reasoning_content: replay.reasoning_content }
      : replay)
  }

  private rememberReplay(
    message: Pick<Message, "content" | "toolCalls">,
    replay: ProviderReplay | undefined,
  ): void {
    if (replay) this.replayStore.set(assistantReplayKey(message), replay)
  }

  private adapterInput(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): CanonicalAdapterInput {
    const endpoint = endpointProfiles[this.dialect.endpointId]
    const resolved = this.resolvedRuntime ?? {
      identity: {
        providerId: this.dialect.providerId,
        modelId: this.model,
        endpointId: this.dialect.endpointId,
        protocol: "openai-chat",
      },
      model: {
        id: `${this.dialect.providerId}/${this.model}`,
        providerId: this.dialect.providerId,
        kind: "generation",
        intrinsic: {},
      },
      endpoint,
      adapter: this,
      effectiveCapabilities: compatibilityCapabilities(),
    } as unknown as ResolvedOpenAIChatRuntime
    return normalizeCanonicalAdapterInput({
      context,
      tools,
      resolved,
      extensions,
      replayForMessage: message => this.peekProviderReplay(message),
    })
  }

  async complete(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): Promise<Message> {
    const provider = this.dialect.providerId
    if (this.circuit.isOpen()) throw circuitOpenError(provider)
    let input: CanonicalAdapterInput
    let plan: ReturnType<OpenAIChatAdapter["buildRequest"]>
    try {
      input = this.adapterInput(context, tools, extensions)
      plan = this.chat.buildRequest(input, this.dialect)
    } catch (error) {
      throw classifyProviderError(provider, error)
    }
    let lastError: unknown
    for (let attempt = 0; attempt < this.maxRetries; attempt++) {
      try {
        const response = await this.client.chat.completions.create(
          plan.params as unknown as OpenAI.ChatCompletionCreateParamsNonStreaming,
        )
        this.circuit.recordSuccess()
        const decoded = this.chat.decodeComplete(
          response as unknown as Record<string, any>,
          { input },
          this.dialect,
        )
        this.rememberReplay(decoded.message, decoded.replay)
        return decoded.message
      } catch (error) {
        lastError = error
        this.circuit.recordFailure()
        if (attempt < this.maxRetries - 1) {
          await new Promise(resolve => setTimeout(resolve, this.baseDelay * 2 ** attempt))
        }
      }
    }
    throw classifyProviderError(provider, lastError)
  }

  async *stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    _state?: ProviderRunState,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const provider = this.dialect.providerId
    try {
      const input = this.adapterInput(context, tools, extensions)
      const plan = this.chat.buildRequest(input, this.dialect)
      const state = this.chat.createStreamState({ input }, this.dialect)
      const stream = await this.client.chat.completions.create({
        ...plan.params,
        stream: true,
        stream_options: { include_usage: true },
      } as unknown as OpenAI.ChatCompletionCreateParamsStreaming, signal ? { signal } : undefined)
      for await (const chunk of stream as unknown as AsyncIterable<OpenAIChatStreamChunk>) {
        const output = this.chat.pushStreamChunk(chunk, state)
        if (output.replay) {
          this.rememberReplay({
            content: state.accumulatedContent,
            toolCalls: Object.values(state.toolCallBuffers).map(call => ({
              id: call.id,
              name: call.name,
              arguments: call.argsBuffer || "{}",
            })),
          }, output.replay)
        }
        for (const event of output.events) yield event
      }
      const final = this.chat.finishStream(state)
      for (const event of final.events) yield event
      this.rememberReplay({
        content: state.accumulatedContent,
        toolCalls: Object.values(state.toolCallBuffers).map(call => ({
          id: call.id,
          name: call.name,
          arguments: call.argsBuffer || "{}",
        })),
      }, final.replay)
    } catch (error) {
      throw classifyProviderError(provider, error)
    }
  }

  // Compatibility-only white-box seams. Runtime request shaping uses the dialect through adapter.
  protected prepareExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return this.dialect.prepareExtensions(extensions ?? {})
  }

  protected requestBodyExtras(extensions?: Record<string, unknown>): Record<string, unknown> {
    const prepared = this.dialect.prepareExtensions(extensions ?? {})
    return Object.fromEntries(Object.entries(prepared).filter(([key]) =>
      key === "extra_body" || key === "reasoning_effort" || key === "reasoning_split"))
  }

  protected serverTools(extensions?: Record<string, unknown>): unknown[] {
    return this.dialect.serverTools?.(extensions ?? {}) ?? []
  }

  protected requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    const prepared = this.dialect.prepareExtensions(extensions ?? {})
    return omitExtensionKeys(prepared, [
      "model", "messages", "tools", "stream", "stream_options", "extra_body",
      "reasoning_effort", "reasoning_split", "__deepstrikeThinkingEnabled",
    ])
  }

  protected promptCacheKey(context: RenderedContext, tools: ToolSchema[]): string {
    return stablePromptCacheKey([context.systemText, tools.map(tool => tool.name).join(",")])
  }

  protected rememberCompleteReplay(
    _content: string,
    _toolCalls: Array<{ id: string; name: string; arguments: string }>,
    _reasoning: OpenAIChatTurnReasoning,
  ): void {}

  protected rememberStreamReplay(
    _content: string,
    _toolCalls: Array<{ id: string; name: string; arguments: string }>,
    _reasoning: OpenAIChatTurnReasoning,
  ): void {}
}

function compatibilityCapabilities(): ResolvedOpenAIChatRuntime["effectiveCapabilities"] {
  const unknown = { state: "unknown" as const, evidence: [] }
  const unsupported = { state: "unsupported" as const, evidence: ["protocol" as const] }
  return {
    inputModalities: { text: unknown, image: unknown, audio: unknown, video: unsupported, file: unsupported },
    outputModalities: { text: unknown, image: unsupported, audio: unsupported, embedding: unsupported },
    tools: unknown,
    reasoning: unknown,
    parallelToolCalls: unknown,
    structuredOutput: unknown,
    promptCaching: unknown,
    nativeTokenCounting: unknown,
    mediaForms: {
      imageUrl: unknown,
      imageBase64: unknown,
      fileId: unsupported,
      audioUrl: unsupported,
      audioBase64: unknown,
    },
  }
}

export { OpenAIChatProvider as OpenAIProvider }
