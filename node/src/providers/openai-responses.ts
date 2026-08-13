import OpenAI from "openai"
import type {
  LLMProvider,
  Message,
  ProviderRunState,
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

export class OpenAIResponsesProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly responses = new OpenAIResponsesAdapter()
  private readonly resolvedRuntimePolicy: RuntimePolicy
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
  ): Promise<Message> {
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
        const response = await this.client.responses.create(
          plan.params as unknown as OpenAI.Responses.ResponseCreateParamsNonStreaming,
        )
        this.circuit.recordSuccess()
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

  async *stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    try {
      const runState = this.asRunState(state)
      const input = this.adapterInput(context, tools, extensions)
      const plan = this.responses.buildRequest(input, runState)
      const streamState = this.responses.createStreamState({ input }, runState)
      const stream = await this.client.responses.create(
        { ...plan.params, stream: true } as unknown as OpenAI.Responses.ResponseCreateParamsStreaming,
        signal ? { signal } : undefined,
      )

      for await (const chunk of stream as unknown as AsyncIterable<OpenAIResponsesStreamChunk>) {
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
