import { GoogleGenerativeAI, type Content, type RequestOptions } from "@google/generative-ai"
import type { Message, RenderedContext, ToolSchema, StreamEvent, LLMProvider, RuntimePolicy, PromptMeasurement } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker } from "./base.js"
import { endpointProfiles } from "./endpoints.js"
import {
  normalizeCanonicalAdapterInput,
  normalizeCanonicalContext,
  type CanonicalAdapterInput,
} from "./content-normalization.js"
import { GeminiAdapter, canonicalGeminiContents, geminiVendorConfig } from "./gemini-adapter.js"
import { circuitOpenError, classifyProviderError } from "./provider-error.js"

type ResolvedGeminiRuntime = CanonicalAdapterInput["resolved"]

const GEMINI_BASE = (endpointProfiles as Record<string, { baseURL: string }>)["gemini.google"].baseURL

export function buildContents(turns: Message[]): Content[] {
  return canonicalGeminiContents(normalizeCanonicalContext({ systemText: "", turns }))
}

export class GeminiProvider implements LLMProvider {
  private genAI: GoogleGenerativeAI
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number
  private requestOptions: RequestOptions
  private readonly resolvedRuntimePolicy: RuntimePolicy
  private readonly adapter = new GeminiAdapter()

  constructor(
    apiKey: string,
    private readonly model = "gemini-2.0-flash",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL: string = GEMINI_BASE,
    runtimePolicy: RuntimePolicy = {},
    private resolvedRuntime?: ResolvedGeminiRuntime,
  ) {
    this.genAI = withServerRuntimeGuard(() => new GoogleGenerativeAI(apiKey))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
    this.requestOptions = { baseUrl: baseURL }
    this.resolvedRuntimePolicy = runtimePolicy
  }

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  bindResolvedRuntime(resolved: ResolvedGeminiRuntime): void {
    if (
      resolved.identity.protocol !== "gemini"
      || resolved.identity.modelId !== this.model
    ) {
      throw new Error("GeminiProvider received a mismatched resolved runtime")
    }
    this.resolvedRuntime = resolved
  }

  private adapterInput(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): CanonicalAdapterInput {
    if (!this.resolvedRuntime) {
      // Direct class construction is a published compatibility path. A-07 replaces it with
      // injected runtime profiles; until then this local descriptor contains no Registry lookup.
      const resolved = {
        identity: {
          providerId: "gemini",
          modelId: this.model,
          endpointId: "gemini.google",
          protocol: "gemini",
        },
        model: { id: `gemini/${this.model}`, providerId: "gemini", kind: "generation", intrinsic: {} },
        endpoint: endpointProfiles["gemini.google"],
        adapter: this,
        effectiveCapabilities: {
          inputModalities: Object.fromEntries(["text", "image", "audio", "video", "file"].map(
            modality => [modality, { state: modality === "video" || modality === "file" ? "unsupported" : "unknown", evidence: [] }],
          )),
          outputModalities: Object.fromEntries(["text", "image", "audio", "embedding"].map(
            modality => [modality, { state: "unknown", evidence: [] }],
          )),
          tools: { state: "unknown", evidence: [] },
          reasoning: { state: "unknown", evidence: [] },
          parallelToolCalls: { state: "unknown", evidence: [] },
          structuredOutput: { state: "unknown", evidence: [] },
          promptCaching: { state: "unknown", evidence: [] },
          nativeTokenCounting: { state: "unknown", evidence: [] },
          mediaForms: Object.fromEntries(
            ["imageUrl", "imageBase64", "fileId", "audioUrl", "audioBase64"].map(
              form => [form, { state: "unknown", evidence: [] }],
            ),
          ),
        },
      } as unknown as ResolvedGeminiRuntime
      return normalizeCanonicalAdapterInput({ context, tools, resolved, extensions })
    }
    return normalizeCanonicalAdapterInput({
      context,
      tools,
      resolved: this.resolvedRuntime,
      extensions,
    })
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw circuitOpenError("gemini")
    let input: CanonicalAdapterInput
    let plan: ReturnType<GeminiAdapter["buildRequest"]>
    try {
      input = this.adapterInput(context, tools, extensions)
      plan = this.adapter.buildRequest(input)
    } catch (error) {
      throw classifyProviderError("gemini", error)
    }

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const m = this.genAI.getGenerativeModel(plan.modelParams, this.requestOptions)
        const resp = await m.generateContent(plan.request)
        this.circuit.recordSuccess()
        return this.adapter.decodeComplete(resp.response, { input }).message
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw classifyProviderError("gemini", lastErr)
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    try {
      const input = this.adapterInput(context, tools, extensions)
      const plan = this.adapter.buildRequest(input)
      const m = this.genAI.getGenerativeModel(plan.modelParams, this.requestOptions)
      const result = await m.generateContentStream(plan.request)
      const state = this.adapter.createStreamState({ input })

      for await (const chunk of result.stream) {
        for (const event of this.adapter.pushStreamChunk(chunk, state).events) yield event
      }

      for (const event of this.adapter.finishStream(state, await result.response).events) yield event
    } catch (error) {
      throw classifyProviderError("gemini", error)
    }
  }

  /**
   * spc_011-C-05: preflight native token count via `GenerativeModel.countTokens` — reuses the
   * same contents/tools/vendorConfig construction `complete()` uses so the counted request and
   * the sent request never diverge.
   */
  async countTokens(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<PromptMeasurement> {
    const plan = this.adapter.buildRequest(this.adapterInput(context, tools, extensions))
    const m = this.genAI.getGenerativeModel(plan.modelParams, this.requestOptions)
    const resp = await m.countTokens(plan.request)
    return {
      inputTokens: resp.totalTokens,
      source: { kind: "native", provider: "gemini" },
      confidence: "exact",
    }
  }

  /**
   * Gemini vendor features from extensions, mapped to the Node SDK shape (mirrors the Python provider's
   * extension keys for a consistent cross-SDK API):
   *  - `google_search` (truthy → default, object → config): Google Search grounding server tool
   *    (gemini-2.0+), appended to tools[].
   *  - `response_mime_type` / `response_schema`: structured output → `generationConfig` (the API rejects
   *    pairing this with google_search).
   */
  vendorConfig(extensions?: Record<string, unknown>): { tools?: unknown[]; generationConfig?: Record<string, unknown> } {
    return geminiVendorConfig(extensions ?? {})
  }
}
