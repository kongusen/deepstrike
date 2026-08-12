import { readFile } from "node:fs/promises"
import { join } from "node:path"

// spc_007-01: minimal OpenAI Agents SDK-shaped agent definition, used by spc_007-02's
// `fromOpenAiAgent` adapter test. Pure data-prep card — this smoke test only proves the fixture
// exists, parses, and carries the fields the doc's §2 table maps.
describe("spc_007-01: OpenAI-style agent fixture", () => {
  it("parses and contains the required fields", async () => {
    const path = join(process.cwd(), "src", "__fixtures__", "openai-agent.json")
    const raw = await readFile(path, "utf8")
    const fixture = JSON.parse(raw)

    expect(typeof fixture.name).toBe("string")
    expect(typeof fixture.instructions).toBe("string")
    expect(Array.isArray(fixture.tools)).toBe(true)
    expect(Array.isArray(fixture.handoffs)).toBe(true)
  })
})
