import { readFileSync } from "node:fs"
import { join } from "node:path"

test("SPC-028-47/49 package declares runtime and evals subpaths", () => {
  const packageJson = JSON.parse(readFileSync(join(process.cwd(), "package.json"), "utf8")) as { exports: Record<string, unknown> }
  expect(packageJson.exports["./runtime"]).toBeDefined()
  expect(packageJson.exports["./evals"]).toBeDefined()
})

test("SPC-028-46 root keeps runtime internals behind subpaths", () => {
  const root = readFileSync(join(process.cwd(), "src/index.ts"), "utf8")
  for (const forbiddenModule of ["./runtime/", "./agent-ir.js", "./signals/"]) {
    expect(root).not.toContain(`from \"${forbiddenModule}`)
  }
  for (const internal of [
    "KernelJournal",
    "ProviderRequestPlan",
    "ProviderAttempt",
    "ContextPrepared",
    "EvolutionRuntime",
    "ReactiveSession",
    "InMemoryEventStream",
    "InMemoryGroupBudgetStore",
    "VerifiableOperation",
    "AgentDefinition",
    "RuntimeBinding",
  ]) {
    expect(root).not.toMatch(new RegExp(`\\b${internal}\\b`))
  }
})
