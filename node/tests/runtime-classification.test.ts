import { readFileSync } from "node:fs"
import { join } from "node:path"
import { RUNTIME_OBJECT_CLASSIFICATIONS } from "../src/runtime-classification.js"

test("SPC-028-02 classification registry matches the shared fixture", () => {
  expect(RUNTIME_OBJECT_CLASSIFICATIONS).toEqual(JSON.parse(readFileSync(
    join(process.cwd(), "../tests/fixtures/runtime-language/object-classification.json"), "utf8",
  )))
})

test("SPC-028-02 every required core object has a complete classification", () => {
  const required = [
    "AgentDefinition", "AgentSpec", "StoredMessageState", "ProviderRequestPlan", "PromptMeasurement",
    "ProviderUsage", "ResolvedProviderRoute", "ProviderAttempt", "KernelInput", "KernelEffect",
    "BudgetLedger", "Journal", "Checkpoint", "SessionLog", "EvaluationRun",
  ] as const
  for (const name of required) {
    const entry = RUNTIME_OBJECT_CLASSIFICATIONS[name]
    for (const field of ["domain", "authority", "representation", "durability", "identity", "causation", "replay"] as const) {
      expect(entry[field]).toEqual(expect.any(String))
      expect(entry[field]).not.toBe("")
    }
  }
})
