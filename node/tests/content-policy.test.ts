import {
  contentDispositionFor,
  ContentPolicyError,
} from "../src/providers/content-policy.js"
import { normalizeCanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"
import type { RenderedContext } from "../src/types.js"

describe("spc_015-06 document/video content policy", () => {
  it("declares native, bridge, and unsupported outcomes by protocol and placement", () => {
    expect(contentDispositionFor("anthropic-messages", "image", "tool_result")).toBe("native")
    expect(contentDispositionFor("openai-responses", "image", "tool_result")).toBe("native")
    expect(contentDispositionFor("openai-chat", "file", "tool_result")).toBe("bridge")
    expect(contentDispositionFor("gemini", "audio", "tool_result")).toBe("bridge")
    expect(contentDispositionFor("openai-responses", "file", "message")).toBe("native")
    expect(contentDispositionFor("anthropic-messages", "video", "message")).toBe("unsupported")
    expect(contentDispositionFor("ollama-chat", "file", "message")).toBe("unsupported")
  })

  it("rejects a protocol-unsupported video before any serializer can flatten it", () => {
    const resolved = resolveProviderRuntime({ model: "anthropic/claude-sonnet-4-6", apiKey: "test" })
    const context: RenderedContext = {
      systemText: "",
      turns: [{
        role: "tool",
        content: "[video]",
        toolCalls: [],
        contentParts: [{
          type: "tool_result",
          callId: "call_video",
          output: "[video]",
          isError: false,
          contentParts: [{
            type: "video",
            source: { kind: "url", url: "https://example.test/clip.mp4" },
            mediaType: "video/mp4",
          }],
        }],
      }],
    }

    expect(() => normalizeCanonicalAdapterInput({ context, tools: [], resolved })).toThrow(ContentPolicyError)
    expect(() => normalizeCanonicalAdapterInput({ context, tools: [], resolved })).toThrow(/Unsupported content policy: video/)
  })
})
