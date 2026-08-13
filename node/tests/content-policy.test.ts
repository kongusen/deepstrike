import {
  contentDispositionFor,
  ContentPolicyError,
} from "../src/providers/content-policy.js"
import { normalizeCanonicalAdapterInput } from "../src/providers/content-normalization.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import type { RenderedContext } from "../src/types.js"
import { readFileSync } from "node:fs"
import { join } from "node:path"

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

  it("allows an explicitly bridged file result and keeps its visible projection on the OpenAI chat wire", () => {
    const resolved = resolveProviderRuntime({ model: "openai/gpt-4o", apiKey: "test" })
    const context: RenderedContext = {
      systemText: "",
      turns: [{
        role: "assistant",
        content: "",
        toolCalls: [{ id: "call_file", name: "read_report", arguments: "{}" }],
      }, {
        role: "tool",
        content: "[file]",
        toolCalls: [],
        contentParts: [{
          type: "tool_result",
          callId: "call_file",
          output: "[file]",
          isError: false,
          contentParts: [{
            type: "file",
            source: { kind: "url", url: "https://example.test/report.pdf" },
            mediaType: "application/pdf",
          }],
        }],
      }],
    }

    const input = normalizeCanonicalAdapterInput({ context, tools: [], resolved })
    const plan = new OpenAIChatAdapter().buildRequest(input)
    expect(plan.params.messages).toEqual([{
      role: "assistant",
      content: "",
      tool_calls: [{
        id: "call_file",
        type: "function",
        function: { name: "read_report", arguments: "{}" },
      }],
    }, {
      role: "tool",
      tool_call_id: "call_file",
      content: "[file]",
    }])
  })

  it("matches the shared cross-SDK content policy fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/provider-content-policy/v1.json"), "utf8")) as {
      cases: Array<{ protocol: string; modality: "text" | "image" | "audio" | "video" | "file"; placement: "message" | "tool_result"; disposition: string }>
    }
    for (const testCase of fixture.cases) {
      expect(contentDispositionFor(testCase.protocol, testCase.modality, testCase.placement)).toBe(testCase.disposition)
    }
  })
})
