export type InputModality = "text" | "image" | "audio" | "video" | "file"
export type OutputModality = "text" | "image" | "audio" | "embedding"

export type GenerationProtocol =
  | "anthropic-messages"
  | "openai-chat"
  | "openai-responses"
  | "gemini"
  | "ollama-chat"

export interface ProtocolRuntimeCapabilities {
  acceptedInputModalities: readonly InputModality[]
  emittedOutputModalities: readonly OutputModality[]
  tools: boolean
  parallelToolCalls?: boolean
  structuredOutput?: boolean
  reasoningReplay: "none" | "optional" | "required"
  promptCaching?: boolean
  mediaForms: {
    imageUrl?: boolean
    imageBase64?: boolean
    fileId?: boolean
    audioUrl?: boolean
    audioBase64?: boolean
  }
}

export interface ProtocolRuntimeCapabilityOverrides {
  acceptedInputModalities?: readonly InputModality[]
  emittedOutputModalities?: readonly OutputModality[]
  tools?: boolean
  parallelToolCalls?: boolean
  structuredOutput?: boolean
  reasoningReplay?: ProtocolRuntimeCapabilities["reasoningReplay"]
  promptCaching?: boolean
  mediaForms?: Partial<ProtocolRuntimeCapabilities["mediaForms"]>
}

export const ANTHROPIC_PROTOCOL_CAPABILITIES: ProtocolRuntimeCapabilities = {
  acceptedInputModalities: ["text", "image"],
  emittedOutputModalities: ["text"],
  tools: true,
  parallelToolCalls: true,
  structuredOutput: false,
  reasoningReplay: "required",
  promptCaching: true,
  mediaForms: { imageUrl: true, imageBase64: true },
}

export const GEMINI_PROTOCOL_CAPABILITIES: ProtocolRuntimeCapabilities = {
  acceptedInputModalities: ["text", "image", "audio"],
  emittedOutputModalities: ["text"],
  tools: true,
  reasoningReplay: "none",
  mediaForms: { imageUrl: true, imageBase64: true, audioBase64: true },
}

export const OLLAMA_PROTOCOL_CAPABILITIES: ProtocolRuntimeCapabilities = {
  acceptedInputModalities: ["text", "image"],
  emittedOutputModalities: ["text"],
  tools: true,
  reasoningReplay: "none",
  mediaForms: { imageBase64: true },
}

export const OPENAI_RESPONSES_PROTOCOL_CAPABILITIES: ProtocolRuntimeCapabilities = {
  acceptedInputModalities: ["text", "image", "file"],
  emittedOutputModalities: ["text"],
  tools: true,
  parallelToolCalls: true,
  structuredOutput: true,
  reasoningReplay: "optional",
  promptCaching: true,
  mediaForms: { imageUrl: true, imageBase64: true, fileId: true },
}
