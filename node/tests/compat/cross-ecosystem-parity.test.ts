import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { Agent } from "../../src/agent.js"
import { lowerAgent } from "../../src/agent-ir.js"
import { fromOpenAiAgent, type OpenAiAgentJson } from "../../src/compat/openai/agent.js"
import { tool } from "../../src/tools/index.js"

// spc_007-04: the same intent (name/instructions/one tool), reached via two different entry
// points — native `new Agent(...)` and the OpenAI adapter — must lower to the same public
// AgentSpec shape on common fields, while each side's vendor-specific extras survive under
// `providerOptions`, never silently dropped.
describe("spc_007-04: cross-ecosystem parity via lowerAgent", () => {
  it("native Agent and fromOpenAiAgent agree on common AgentSpec fields", async () => {
    const raw = await readFile(join(process.cwd(), "src", "__fixtures__", "openai-agent.json"), "utf8")
    const openaiJson = JSON.parse(raw) as OpenAiAgentJson & { guardrails?: Array<{ name: string }> }

    const nativeAgent = new Agent({
      name: openaiJson.name,
      instructions: openaiJson.instructions,
      tools: [tool("web_search", "Search the web for information", { type: "object", properties: {} }, async () => "ok")],
      providerOptions: { custom: { note: "native" } },
    })
    const openaiAgent = fromOpenAiAgent(openaiJson)

    const nativeSpec = lowerAgent(nativeAgent)
    const openaiSpec = lowerAgent(openaiAgent)

    expect(openaiSpec.name).toBe(nativeSpec.name)
    expect(openaiSpec.instructions).toBe(nativeSpec.instructions)
    expect(openaiSpec.capabilities.length).toBe(nativeSpec.capabilities.length)

    // Each side's vendor-specific extras must survive under providerOptions, not be dropped.
    expect(nativeSpec.providerOptions).toEqual({ custom: { note: "native" } })
    expect(openaiSpec.providerOptions).toEqual({ openai: { guardrails: openaiJson.guardrails } })
  })
})
