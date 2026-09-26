import { readFileSync } from "node:fs"
import { join } from "node:path"
import { RUNTIME_VOCABULARY, runtimeVocabularyTerms } from "../src/runtime-language.js"

const fixturePath = join(process.cwd(), "../tests/fixtures/runtime-language/vocabulary.json")

test("SPC-028-01 registry matches the shared vocabulary fixture", () => {
  expect(RUNTIME_VOCABULARY).toEqual(JSON.parse(readFileSync(fixturePath, "utf8")))
})

test("SPC-028-01 assigns every vocabulary term to exactly one primary domain", () => {
  const domains = [RUNTIME_VOCABULARY.public, RUNTIME_VOCABULARY.host, RUNTIME_VOCABULARY.kernel, RUNTIME_VOCABULARY.provider]
  const occurrences = new Map<string, number>()
  for (const domain of domains) for (const term of domain) occurrences.set(term, (occurrences.get(term) ?? 0) + 1)
  for (const [term, count] of occurrences) {
    if (count > 1) expect(RUNTIME_VOCABULARY.primaryDomain).toHaveProperty(term)
  }
  expect(Object.keys(RUNTIME_VOCABULARY.primaryDomain).sort()).toEqual(["Capability", "Model", "Route", "Usage"])
  expect(runtimeVocabularyTerms().length).toBe(domains.reduce((sum, domain) => sum + domain.length, 0))
})

test("SPC-028-01 keeps documentation on the same vocabulary version and terms", () => {
  const docs = [
    join(process.cwd(), "../docs/architecture/runtime-language.md"),
    join(process.cwd(), "../docs/en/architecture/runtime-language.md"),
  ].map(path => readFileSync(path, "utf8"))
  for (const doc of docs) {
    expect(doc).toContain(`0.2.75`)
    for (const term of runtimeVocabularyTerms()) expect(doc).toContain(`**${term}**`)
    for (const verb of Object.keys(RUNTIME_VOCABULARY.verbs)) expect(doc).toContain(`**${verb}**`)
  }
})

test("SPC-028-01 rejects public terms that are absent from the registry", () => {
  const docs = [
    join(process.cwd(), "../docs/architecture/runtime-language.md"),
    join(process.cwd(), "../docs/en/architecture/runtime-language.md"),
  ].map(path => readFileSync(path, "utf8"))
  for (const doc of docs) {
    expect(doc).not.toContain("**AgentDefinition**")
    expect(doc).not.toContain("**AgentRuntime**")
  }
})
