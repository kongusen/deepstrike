import { AnthropicProvider } from "../src/providers/anthropic.js"
import type { RenderedContext } from "../src/types.js"

describe("Anthropic Prompt Caching", () => {
  it("injects cache_control on systemStable and systemKnowledge as separate blocks", async () => {
    const provider = new AnthropicProvider("test-key")
    let capturedParams: any = null

    ;(provider as any).client = {
      messages: {
        create: async (params: any) => {
          capturedParams = params
          return {
            content: [{ type: "text", text: "hello" }],
            usage: { input_tokens: 100, output_tokens: 20 },
          }
        },
      },
    }

    const context: RenderedContext = {
      systemText: "system rules\nskill: debug",
      systemStable: "system rules",
      systemKnowledge: "skill: debug",
      turns: [
        { role: "user", content: "[TASK STATE] goal: do it\n\nProceed." },
        { role: "assistant", content: "first assistant message" },
        { role: "user", content: "second user message" },
      ],
    }

    const tools = [
      { name: "tool1", description: "first tool", parameters: "{}" },
      { name: "tool2", description: "second tool", parameters: "{}" },
    ]

    await provider.complete(context, tools)

    expect(capturedParams).toBeDefined()

    // systemStable and systemKnowledge are separate cache blocks
    expect(capturedParams.system).toEqual([
      { type: "text", text: "system rules", cache_control: { type: "ephemeral" } },
      { type: "text", text: "skill: debug", cache_control: { type: "ephemeral" } },
    ])

    // last tool has cache_control
    expect(capturedParams.tools[1]).toMatchObject({ cache_control: { type: "ephemeral" } })

    // messages array contains turns as-is (no systemVolatile injection)
    expect(capturedParams.messages).toHaveLength(3)
    expect(capturedParams.messages[2]).toEqual({ role: "user", content: "second user message" })
  })

  it("handles empty systemKnowledge cleanly", async () => {
    const provider = new AnthropicProvider("test-key")
    let capturedParams: any = null

    ;(provider as any).client = {
      messages: {
        create: async (params: any) => {
          capturedParams = params
          return {
            content: [{ type: "text", text: "hello" }],
            usage: { input_tokens: 50, output_tokens: 10 },
          }
        },
      },
    }

    const context: RenderedContext = {
      systemText: "system rules",
      systemStable: "system rules",
      turns: [{ role: "user", content: "single message" }],
    }

    await provider.complete(context, [])

    expect(capturedParams.system).toEqual([
      { type: "text", text: "system rules", cache_control: { type: "ephemeral" } },
    ])
    expect(capturedParams.messages).toEqual([{ role: "user", content: "single message" }])
  })
})
