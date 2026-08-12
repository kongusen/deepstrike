import type { Message, RenderedContext, ToolSchema, StreamEvent, LLMProvider, RuntimePolicy } from "../types.js"
import {
  normalizeCanonicalAdapterInput,
  type CanonicalAdapterInput,
} from "./content-normalization.js"
import { endpointProfiles } from "./endpoints.js"
import { OllamaAdapter, type OllamaChunk } from "./ollama-adapter.js"
import { classifyProviderError, ProviderError } from "./provider-error.js"

type ResolvedOllamaRuntime = CanonicalAdapterInput["resolved"]

export class OllamaProvider implements LLMProvider {
  private readonly adapter = new OllamaAdapter()

  constructor(
    private readonly model = "llama3",
    private readonly baseUrl = "http://localhost:11434",
    private readonly resolvedRuntimePolicy: RuntimePolicy = {},
    private resolvedRuntime?: ResolvedOllamaRuntime,
  ) {}

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  bindResolvedRuntime(resolved: ResolvedOllamaRuntime): void {
    if (
      resolved.identity.protocol !== "ollama-chat"
      || resolved.identity.modelId !== this.model
    ) {
      throw new Error("OllamaProvider received a mismatched resolved runtime")
    }
    this.resolvedRuntime = resolved
  }

  private adapterInput(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
  ): CanonicalAdapterInput {
    const resolved = this.resolvedRuntime ?? {
      identity: {
        providerId: "ollama",
        modelId: this.model,
        endpointId: "ollama.local",
        protocol: "ollama-chat",
      },
      model: { id: `ollama/${this.model}`, providerId: "ollama", kind: "generation", intrinsic: {} },
      endpoint: endpointProfiles["ollama.local"],
      adapter: this,
      effectiveCapabilities: {
        inputModalities: Object.fromEntries(["text", "image", "audio", "video", "file"].map(
          modality => [modality, { state: modality === "audio" || modality === "video" || modality === "file" ? "unsupported" : "unknown", evidence: [] }],
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
    } as unknown as ResolvedOllamaRuntime
    return normalizeCanonicalAdapterInput({ context, tools, resolved, extensions })
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    try {
      const input = this.adapterInput(context, tools, extensions)
      const body = { ...this.adapter.buildRequest(input), stream: false }
      const resp = await fetch(`${this.baseUrl}/api/chat`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      })
      if (!resp.ok) throw ollamaHttpError(resp.status)
      const data = await resp.json() as OllamaChunk
      return this.adapter.decodeComplete(data, { input }).message
    } catch (error) {
      throw classifyProviderError("ollama", error)
    }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    try {
      const input = this.adapterInput(context, tools, extensions)
      const body = { ...this.adapter.buildRequest(input), stream: true }
      const resp = await fetch(`${this.baseUrl}/api/chat`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      })
      if (!resp.ok) throw ollamaHttpError(resp.status)
      if (!resp.body) {
        throw new ProviderError({
          provider: "ollama",
          kind: "protocol",
          retryable: false,
          message: "Ollama stream response has no body",
        })
      }
      const reader = resp.body.getReader()
      const decoder = new TextDecoder()
      const ndjson = this.adapter.createNdjsonDecoder()
      const state = this.adapter.createStreamState({ input })
      while (true) {
        const { done, value } = await reader.read()
        if (done) break
        for (const chunk of ndjson.push(decoder.decode(value, { stream: true }))) {
          for (const event of this.adapter.pushStreamChunk(chunk, state).events) yield event
        }
      }
      for (const chunk of ndjson.finish(decoder.decode())) {
        for (const event of this.adapter.pushStreamChunk(chunk, state).events) yield event
      }
      for (const event of this.adapter.finishStream(state, state.finalChunk).events) yield event
    } catch (error) {
      throw classifyProviderError("ollama", error)
    }
  }
}

function ollamaHttpError(status: number): ProviderError {
  return classifyProviderError("ollama", Object.assign(
    new Error(`Ollama error: ${status}`),
    { status },
  ))
}
