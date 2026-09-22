import { readFileSync } from "node:fs"
import { join } from "node:path"
import { GENERATION_PROTOCOLS } from "../src/providers/protocol-capabilities.js"
import { readdirSync, readFileSync as readSource } from "node:fs"
import { join as joinPath } from "node:path"

test("SPC-028-10 has one GenerationProtocol authority", () => {
  const source = readFileSync(join(process.cwd(), "src/providers/protocol-capabilities.ts"), "utf8")
  expect(source.match(/export type GenerationProtocol/g)?.length).toBe(1)
  expect(source).not.toContain("ProviderProtocol")
  expect(GENERATION_PROTOCOLS).toEqual(expect.arrayContaining(["anthropic-messages", "openai-chat", "openai-responses", "gemini", "ollama-chat"]))
})

test("SPC-028-10 scans every SDK surface for the retired ProviderProtocol vocabulary", () => {
  const roots = ["src", "../wasm/src", "../python/deepstrike"]
  const files: string[] = []
  const visit = (root: string) => {
    for (const entry of readdirSync(joinPath(process.cwd(), root), { withFileTypes: true })) {
      const path = joinPath(root, entry.name)
      if (entry.isDirectory()) visit(path)
      else if (/\.(ts|py)$/.test(entry.name)) files.push(path)
    }
  }
  for (const root of roots) visit(root)
  const offenders = files.filter((file) => /\bProviderProtocol\b/.test(readSource(joinPath(process.cwd(), file), "utf8")))
  expect(offenders).toEqual([])
})
