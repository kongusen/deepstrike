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

test("SPC-028-05 normalization accepts the facade definition with its default identity", async () => {
  const agent = createAgent({
    provider: new ReplayProvider([{ role: "assistant", content: "done" }]),
    instructions: "Keep the instruction",
    providerOptions: { openai: { temperature: 0 } },
  })
  const spec = lowerAgent(normalizeAgent(agent.definition))
  expect(spec.name).toBe(agent.name)
  expect(spec.instructions).toBe(agent.definition.instructions)
  expect(spec.extensions).toEqual(agent.definition.providerOptions)
  expect(spec).not.toHaveProperty("provider")
  await expect(agent.run("go")).resolves.toMatchObject({ output: "done" })
})
