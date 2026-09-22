import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import ts from "typescript"
import { createAgent } from "../src/agent-facade.js"
import { normalizeAgent, lowerAgent } from "../src/agent-ir.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"

function declarations(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) return declarations(path)
    if (!path.endsWith(".ts")) return []
    const source = ts.createSourceFile(path, readFileSync(path, "utf8"), ts.ScriptTarget.Latest)
    return source.statements.filter(statement =>
      (ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)) &&
      statement.name.text === "AgentDefinition",
    ).map(() => path)
  })
}

test("SPC-028-05 Node declares exactly one public AgentDefinition", () => {
  expect(declarations(join(process.cwd(), "src"))).toEqual([join(process.cwd(), "src/agent-facade.ts")])
})

test("SPC-028-28 executable facade has one public Agent name", () => {
  const source = readFileSync(join(process.cwd(), "src/agent-facade.ts"), "utf8")
  expect(source).toMatch(/export interface Agent\s*\{/)
  expect(source).toMatch(/export type AgentRuntime = Agent/)
  expect(source).not.toMatch(/export interface ExecutableAgent/)
})

test("SPC-028-05 normalization keeps the semantic definition free of runtime binding", async () => {
  const agent = createAgent({
    runtimeBinding: { provider: new ReplayProvider([{ role: "assistant", content: "done" }]) },
    instructions: "Keep the instruction",
    providerOptions: { openai: { temperature: 0 } },
  })
  const spec = lowerAgent(normalizeAgent(agent.definition))
  expect(spec.name).toBe(agent.name)
  expect(spec.instructions).toBe(agent.definition.instructions)
  expect(spec.extensions).toEqual(agent.definition.providerOptions)
  expect(spec).not.toHaveProperty("provider")
  expect(agent.definition).not.toHaveProperty("runtimeBinding")
  await expect(agent.run("go")).resolves.toMatchObject({ output: "done" })
})

test("SPC-028-29 binds host runtime separately from the semantic definition", async () => {
  const agent = createAgent(
    { name: "bound", model: "gpt-5.4" },
    { provider: new ReplayProvider([{ role: "assistant", content: "bound" }]) },
  )
  expect(agent.definition).not.toHaveProperty("runtimeBinding")
  await expect(agent.run("go")).resolves.toMatchObject({ output: "bound" })
})
