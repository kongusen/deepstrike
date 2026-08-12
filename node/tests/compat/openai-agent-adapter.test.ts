import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { fromOpenAiAgent, type OpenAiAgentJson } from "../../src/compat/openai/agent.js"

describe("spc_007-02: fromOpenAiAgent adapter", () => {
  it("maps the OpenAI-style fixture onto an Agent instance", async () => {
    const raw = await readFile(join(process.cwd(), "src", "__fixtures__", "openai-agent.json"), "utf8")
    const fixture = JSON.parse(raw) as OpenAiAgentJson

    const agent = fromOpenAiAgent(fixture)

    expect(agent.name).toBe(fixture.name)
    expect(agent.instructions).toBe(fixture.instructions)
    expect(agent.model).toBe(fixture.model)
    expect(agent.tools?.length).toBe(fixture.tools?.length)
    expect(agent.handoffs).toEqual(fixture.handoffs)
  })
})
