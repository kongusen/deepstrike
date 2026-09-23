import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import ts from "typescript"
import { createAgent } from "../src/agent-facade.js"
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

test("SPC-028-05 facade exposes only its immutable declaration", async () => {
  const agent = createAgent({
    runtimeBinding: { provider: new ReplayProvider([{ role: "assistant", content: "done" }]) },
    instructions: "Keep the instruction",
    providerOptions: { openai: { temperature: 0 } },
  })
  expect(agent.declaration.name).toBe(agent.name)
  expect(agent.declaration.instructions).toBe("Keep the instruction")
  expect(agent.declaration.providerOptions).toEqual({ openai: { temperature: 0 } })
  expect(agent.declaration).not.toHaveProperty("runtimeBinding")
  await expect(agent.run("go")).resolves.toMatchObject({ output: "done" })
})
