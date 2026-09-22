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

test("SPC-028-10 every SDK mirror has the exact canonical protocol set", () => {
  const canonical = [...GENERATION_PROTOCOLS].sort()
  const node = readSource(joinPath(process.cwd(), "src/providers/protocol-capabilities.ts"), "utf8")
  const wasm = readSource(joinPath(process.cwd(), "../wasm/src/types.ts"), "utf8")
  const python = readSource(joinPath(process.cwd(), "../python/deepstrike/providers/protocols.py"), "utf8")
  const extract = (source: string) => [...new Set([...source.matchAll(/\b(?:anthropic-messages|openai-chat|openai-responses|gemini|ollama-chat)\b/g)].map(match => match[0]))].sort()
  expect(extract(node)).toEqual(canonical)
  expect(extract(wasm)).toEqual(canonical)
  expect(extract(python)).toEqual(canonical)
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

test("SPC-028 kernel effect vocabulary has no MeasurePrompt command or tag", () => {
  const files = [
    "../crates/deepstrike-core/src/runtime/kernel/wire/effect.rs",
    "../crates/deepstrike-core/src/runtime/kernel/wire/projection.rs",
    "../crates/deepstrike-core/src/runtime/kernel/wire/driver/effects.rs",
    "../rust/src/runtime/canonical_runner_runtime.rs",
  ]
  const offenders = files.filter((file) => /MeasurePrompt|measure_prompt|PromptMeasured/.test(readSource(joinPath(process.cwd(), file), "utf8")))
  expect(offenders).toEqual([])
})

test("Python provider protocol has one strict shared authority", () => {
  const shared = readSource(joinPath(process.cwd(), "../python/deepstrike/providers/protocols.py"), "utf8")
  const registry = readSource(joinPath(process.cwd(), "../python/deepstrike/providers/model_registry.py"), "utf8")
  const base = readSource(joinPath(process.cwd(), "../python/deepstrike/providers/base.py"), "utf8")
  expect(shared).toContain("GenerationProtocol = Literal[")
  expect(registry).toContain("from .protocols import GenerationProtocol")
  expect(base).toContain("from .protocols import GenerationProtocol")
  expect(base).not.toMatch(/GenerationProtocol\s*=\s*str/)
})

test("SPC-028 cross-SDK Agent surface exposes one executable model-first contract", () => {
  const python = readSource(joinPath(process.cwd(), "../python/deepstrike/agent.py"), "utf8")
  const wasm = readSource(joinPath(process.cwd(), "../wasm/src/agent.ts"), "utf8")
  const rust = readSource(joinPath(process.cwd(), "../rust/src/agent.rs"), "utf8")
  const pythonRoot = readSource(joinPath(process.cwd(), "../python/deepstrike/__init__.py"), "utf8")
  expect(python).toContain("def create_agent")
  expect(python).toContain("async def run")
  expect(python).toContain("async def stream")
  expect(python).toContain("self._binding")
  expect(python).not.toMatch(/self\.runtime_binding\s*=/)
  expect(wasm).toContain("async run(")
  expect(wasm).toContain("stream(")
  expect(wasm).toContain("export function createAgent")
  expect(wasm).toContain("readonly definition")
  expect(wasm).not.toMatch(/readonly runtimeBinding/)
  expect(rust).toContain("pub struct Agent")
  expect(rust).toContain("pub struct AgentDefinition")
  expect(rust).toContain("pub async fn run")
  expect(pythonRoot).toContain('"Agent", "create_agent"')
  expect(pythonRoot).not.toContain('"RuntimeRunner", "RuntimeOptions"')
})
