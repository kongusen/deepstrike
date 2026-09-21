import { readFileSync } from "node:fs"
import { join } from "node:path"
import { GENERATION_PROTOCOLS } from "../src/providers/protocol-capabilities.js"

test("SPC-028-10 has one GenerationProtocol authority", () => {
  const source = readFileSync(join(process.cwd(), "src/providers/protocol-capabilities.ts"), "utf8")
  expect(source.match(/export type GenerationProtocol/g)?.length).toBe(1)
  expect(source).not.toContain("ProviderProtocol")
  expect(GENERATION_PROTOCOLS).toEqual(expect.arrayContaining(["anthropic-messages", "openai-chat", "openai-responses", "gemini", "ollama-chat"]))
})
