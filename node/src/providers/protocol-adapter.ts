import type {
  Message,
  ProviderReplay,
  ProviderRunState,
  ProviderUsage,
  StreamEvent,
} from "../types.js"
import type { CanonicalAdapterInput } from "./content-normalization.js"
import type {
  GenerationProtocol,
  ProtocolRuntimeCapabilities,
} from "./protocol-capabilities.js"

export {
  GEMINI_PROTOCOL_CAPABILITIES,
  OLLAMA_PROTOCOL_CAPABILITIES,
} from "./protocol-capabilities.js"

export type CanonicalStopReason =
  | "end_turn"
  | "tool_use"
  | "max_tokens"
  | "stop_sequence"
  | "content_filter"
  | "other"

export interface AdapterOutput {
  events: StreamEvent[]
  replay?: ProviderReplay
  runStatePatch?: Partial<ProviderRunState>
}

export interface AdapterDecodeInput {
  input: CanonicalAdapterInput
}

export interface AdapterStreamInput {
  input: CanonicalAdapterInput
}

export interface ProtocolAdapter<
  TRequest,
  TCompleteResponse,
  TStreamChunk,
  TStreamState,
  TStreamFinal = undefined,
> {
  readonly protocol: GenerationProtocol
  readonly protocolCapabilities: ProtocolRuntimeCapabilities

  buildRequest(input: CanonicalAdapterInput): TRequest
  decodeComplete(raw: TCompleteResponse, input: AdapterDecodeInput): {
    message: Message
    replay?: ProviderReplay
  }

  createStreamState(input: AdapterStreamInput): TStreamState
  pushStreamChunk(chunk: TStreamChunk, state: TStreamState): AdapterOutput
  finishStream(
    state: TStreamState,
    final: TStreamFinal,
  ): AdapterOutput | Promise<AdapterOutput>

  normalizeUsage(raw: unknown): ProviderUsage | undefined
  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined
}

export class ProtocolResponseError extends Error {
  readonly protocol: GenerationProtocol

  constructor(protocol: GenerationProtocol, message: string) {
    super(`${protocol} protocol response error: ${message}`)
    this.name = "ProtocolResponseError"
    this.protocol = protocol
  }
}
