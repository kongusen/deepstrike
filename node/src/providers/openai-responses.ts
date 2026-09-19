import { requestSnapshot } from "./prepared-request.js"
import type { PreparedProviderRequest, ProviderRunState as PreparedRunState } from "../types.js"
import OpenAI from "openai"
import type {
  LLMProvider,
  ProviderMessage,
  PromptMeasurement,
  ProviderRunState,
  ProviderTransportTelemetry,
  RenderedContext,
  RuntimePolicy,
  StreamEvent,
  ToolSchema,
} from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker } from "./base.js"
import {
  normalizeCanonicalAdapterInput,
  type CanonicalAdapterInput,
} from "./content-normalization.js"
import { endpointProfiles } from "./endpoints.js"
import {
  OpenAIResponsesAdapter,
  type OpenAIResponsesRunState,
  type OpenAIResponsesStreamChunk,
} from "./openai-responses-adapter.js"
import { circuitOpenError, classifyProviderError } from "./provider-error.js"

export { OpenAIResponsesAdapter } from "./openai-responses-adapter.js"
export type {
  OpenAIResponsesRequestPlan,
  OpenAIResponsesRunState,
  OpenAIResponsesStreamState,
} from "./openai-responses-adapter.js"

type ResolvedOpenAIResponsesRuntime = CanonicalAdapterInput["resolved"]

const OFFICIAL_OPENAI_RESPONSES_BASE_URL = "https://api.openai.com/v1"

/** Params the official input-token count endpoint accepts (SDK `InputTokenCountParams`). The
 * create plan is projected onto this set — remaining keys (max_output_tokens, store, …) cannot
 * change the input token count — rather than maintaining a second serialization. */
const INPUT_TOKEN_COUNT_PARAM_KEYS: readonly string[] = [
  "conversation", "input", "instructions", "model", "parallel_tool_calls",
  "previous_response_id", "reasoning", "text", "tool_choice", "tools", "truncation",
]

export class OpenAIResponsesProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly responses = new OpenAIResponsesAdapter()
  private readonly resolvedRuntimePolicy: RuntimePolicy
  private readonly directNativeTokenCounting: boolean
  private resolvedRuntime?: ResolvedOpenAIResponsesRuntime

  constructor(
    apiKey: string,
    protected readonly model = "gpt-4.1",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
    runtimePolicy: RuntimePolicy = {},
    authMode: "api_key" | "bearer" = "api_key",
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({
      apiKey,
      baseURL,
      ...(authMode === "bearer" ? { defaultHeaders: { Authorization: `Bearer ${apiKey}` } } : {}),
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
    this.resolvedRuntimePolicy = runtimePolicy
    this.directNativeTokenCounting = baseURL.replace(/\/+$/, "") === OFFICIAL_OPENAI_RESPONSES_BASE_URL
  }

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  bindResolvedRuntime(resolved: ResolvedOpenAIResponsesRuntime): void {
    if (
      resolved.identity.protocol !== "openai-responses"
      || resolved.identity.providerId !== "openai"
      || resolved.identity.modelId !== this.model
    ) {
      throw new Error("OpenAIResponsesProvider received a mismatched resolved runtime")
    }
    this.resolvedRuntime = resolved
  }

  createRunState(): OpenAIResponsesRunState {
    return { coveredMessageCount: 0 }
  }

  private adapterInput(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): CanonicalAdapterInput {
    const resolved = this.resolvedRuntime ?? {
      identity: {
        providerId: "openai",
        modelId: this.model,
        endpointId: "openai.responses",
        protocol: "openai-responses",
      },
      model: {
        id: `openai/${this.model}`,
        providerId: "openai",
        kind: "generation",
        intrinsic: {},
      },
      endpoint: endpointProfiles["openai.responses"],
      adapter: this,
      effectiveCapabilities: compatibilityCapabilities(),
    } as unknown as ResolvedOpenAIResponsesRuntime
    return normalizeCanonicalAdapterInput({ context, tools, resolved, extensions })
  }

  async complete(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): Promise<ProviderMessage> {
    if (this.circuit.isOpen()) throw circuitOpenError("openai")
    let input: CanonicalAdapterInput
    let plan: ReturnType<OpenAIResponsesAdapter["buildRequest"]>
    try {
      input = this.adapterInput(context, tools, extensions)
      plan = this.responses.buildRequest(input)
    } catch (error) {
      throw classifyProviderError("openai", error)
    }
    let lastError: unknown

    for (let attempt = 0; attempt < this.maxRetries; attempt++) {
      try {
        this.lastTelemetry = { rungs: attempt + 1 }
        const response = await this.client.responses.create(
          plan.params as unknown as OpenAI.Responses.ResponseCreateParamsNonStreaming,
        )
        this.circuit.recordSuccess()
        const responseId = (response as unknown as Record<string, unknown>).id
        if (typeof responseId === "string") this.lastTelemetry = { rungs: attempt + 1, responseId }
        return this.responses.decodeComplete(response as unknown as Record<string, any>, { input }).message
      } catch (error) {
        lastError = error
        this.circuit.recordFailure()
        if (attempt < this.maxRetries - 1) {
          await new Promise(resolve => setTimeout(resolve, this.baseDelay * 2 ** attempt))
        }
      }
    }

    throw classifyProviderError("openai", lastError)
  }

  /** spc_024-05: native preflight via the official Responses input-token count endpoint. Counts
   * the exact create request plan (stateful `previous_response_id` continuation included) —
   * native measurement belongs to the verified official endpoint, not the wire protocol. */
  async countTokens(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
  ): Promise<PromptMeasurement> {
    return this.countPlan(this.responses.buildRequest(this.adapterInput(context, tools, extensions), this.asRunState(state)))
  }

  private async countPlan(plan: ReturnType<OpenAIResponsesAdapter["buildRequest"]>): Promise<PromptMeasurement> {
    const enabled = this.resolvedRuntime
      ? this.resolvedRuntime.effectiveCapabilities.nativeTokenCounting.state === "supported"
      : this.directNativeTokenCounting
    const inputTokens = this.client.responses.inputTokens
    if (!enabled || typeof inputTokens?.count !== "function") {
      throw new Error("Native token counting is unavailable on this OpenAI-compatible endpoint")
    }
    const body = Object.fromEntries(
      INPUT_TOKEN_COUNT_PARAM_KEYS
        .filter(key => key in plan.params)
        .map(key => [key, plan.params[key]]),
    )
    const response = await inputTokens.count(
      requestSnapshot(body) as unknown as Parameters<typeof inputTokens.count>[0],
    )
    return {
      inputTokens: response.input_tokens,
      source: { kind: "native", provider: "openai" },
      confidence: "exact",
    }
  }

  prepareRequest(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: PreparedRunState): PreparedProviderRequest {
    const runState = requestSnapshot(this.asRunState(state))
    const input = this.adapterInput(context, tools, extensions)
    const plan = requestSnapshot(this.responses.buildRequest(input, runState))
    plan.params.stream = true
    return {
      scope: "encoded_body", request: requestSnapshot(plan.params), state: requestSnapshot(state ?? null),
      stream: signal => this.streamPlan(input, plan, runState, state, signal),
      countTokens: () => this.countPlan(plan),
    }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: PreparedRunState, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    yield* this.prepareRequest(context, tools, extensions, state).stream(signal)
  }

  private async *streamPlan(input: CanonicalAdapterInput, plan: ReturnType<OpenAIResponsesAdapter["buildRequest"]>, runState: OpenAIResponsesRunState, state?: ProviderRunState, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    try {
      const streamState = this.responses.createStreamState({ input }, runState)
      const stream = await this.client.responses.create(
        { ...plan.params, stream: true } as unknown as OpenAI.Responses.ResponseCreateParamsStreaming,
        signal ? { signal } : undefined,
      )

      this.lastTelemetry = { rungs: 1 }
      for await (const chunk of stream as unknown as AsyncIterable<OpenAIResponsesStreamChunk>) {
        if (this.lastTelemetry.responseId === undefined
          && (chunk.type === "response.completed" || chunk.type === "response.incomplete" || chunk.type === "response.created")) {
          const id = (chunk as Record<string, any>).response?.id
          if (typeof id === "string") this.lastTelemetry = { rungs: 1, responseId: id }
        }
        const output = this.responses.pushStreamChunk(chunk, streamState)
        for (const event of output.events) yield event
        if (output.runStatePatch) {
          Object.assign(runState, output.runStatePatch)
          if (state) Object.assign(state, output.runStatePatch)
        }
      }

      const final = this.responses.finishStream(streamState)
      for (const event of final.events) yield event
      if (final.runStatePatch) {
        Object.assign(runState, final.runStatePatch)
        if (state) Object.assign(state, final.runStatePatch)
      }
    } catch (error) {
      throw classifyProviderError("openai", error)
    }
  }

  // White-box test seams. Protocol request shaping belongs to the adapter.
  private builtinTools(extensions?: Record<string, unknown>): Record<string, unknown>[] {
    return this.responses.builtinTools(extensions)
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return this.responses.requestExtensions(extensions)
  }

  /** P4-S1: transport facts of the most recent execution. Host evidence only (B7). */
  private lastTelemetry: ProviderTransportTelemetry | undefined

  peekTransportTelemetry(): ProviderTransportTelemetry | undefined {
    return this.lastTelemetry
  }

  private asRunState(state?: ProviderRunState): OpenAIResponsesRunState {
    if (!state) return this.createRunState()
    return {
      ...state,
      coveredMessageCount: typeof state.coveredMessageCount === "number"
        ? state.coveredMessageCount
        : 0,
    } as OpenAIResponsesRunState
  }
}

function compatibilityCapabilities(): ResolvedOpenAIResponsesRuntime["effectiveCapabilities"] {
  const unknown = { state: "unknown" as const, evidence: [] }
  const unsupported = { state: "unsupported" as const, evidence: ["protocol" as const] }
  return {
    inputModalities: {
      text: unknown,
      image: unknown,
      audio: unsupported,
      video: unsupported,
      file: unknown,
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
    structuredOutput: unknown,
    promptCaching: unknown,
    nativeTokenCounting: unknown,
    mediaForms: {
      imageUrl: unknown,
      imageBase64: unknown,
      fileId: unknown,
      audioUrl: unsupported,
      audioBase64: unsupported,
    },
  }
}
