/**
 * spc_011-D-02: invariant 2 (no ContentPart reaches the wire for a modality its target model
 * doesn't advertise) at the MODEL level, layered on top of the pre-existing PROTOCOL-level
 * throws (Anthropic/Gemini/Ollama/OpenAI-Responses already reject audio unconditionally — every
 * tracked model on those wires genuinely lacks audio, so there's no model-level gap there yet).
 * The real, previously-silent gap: `toOpenAIContent` (base.ts) happily encodes `input_audio` for
 * ANY openai-chat-protocol model, even "openai/gpt-4o" whose `ModelCapabilities.inputModalities`
 * (spc_011-D-01) does NOT include "audio" — before this card that request went out over the wire
 * with no local check at all.
 */
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { UnsupportedModalityError } from "../src/providers/base.js"
import type { RenderedContext } from "../src/types.js"

const audioContext: RenderedContext = {
  systemText: "",
  turns: [{
    role: "user",
    content: "",
    contentParts: [{ type: "audio", data: "AAAA", mediaType: "audio/wav" }],
  }],
}

describe("model-level modality preflight (spc_011-D-02)", () => {
  it("rejects audio content before sending when the target model's capabilities don't advertise it", async () => {
    const provider = new OpenAIChatProvider("test-key", "gpt-4o")
    ;(provider as any).client = {
      chat: { completions: { create: async () => { throw new Error("must not reach the network") } } },
    }
    await expect(provider.complete(audioContext, [])).rejects.toThrow(UnsupportedModalityError)
  })

  it("does not silently drop the audio part and continue with a text-only request", async () => {
    const provider = new OpenAIChatProvider("test-key", "gpt-4o")
    let sentBody: any
    ;(provider as any).client = {
      chat: { completions: { create: async (body: any) => {
        sentBody = body
        return { choices: [{ message: { content: "should not get here" } }] }
      } } },
    }
    await expect(provider.complete(audioContext, [])).rejects.toThrow(UnsupportedModalityError)
    expect(sentBody).toBeUndefined()
  })
})
