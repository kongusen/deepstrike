#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs"
import { join, resolve } from "node:path"

const root = resolve(new URL("..", import.meta.url).pathname)
const registry = JSON.parse(readFileSync(join(root, "contracts/vocabulary.json"), "utf8"))
const projection = {
  version: registry.version,
  public: registry.layers.public,
  host: registry.layers.runtime,
  kernel: registry.layers.kernel,
  provider: registry.layers.provider,
  primaryDomain: registry.primaryDomain,
  verbs: registry.verbs,
}
const ts = `/** Generated from contracts/vocabulary.json. Do not edit by hand. */\nexport const RUNTIME_VOCABULARY = ${JSON.stringify(projection, null, 2)} as const\n\nexport type RuntimeLanguage = typeof RUNTIME_VOCABULARY\nexport type RuntimeLanguageDomain = "public" | "host" | "kernel" | "provider"\n\nexport function runtimeVocabularyTerms(): readonly string[] {\n  return [\n    ...RUNTIME_VOCABULARY.public,\n    ...RUNTIME_VOCABULARY.host,\n    ...RUNTIME_VOCABULARY.kernel,\n    ...RUNTIME_VOCABULARY.provider,\n  ]\n}\n`
writeFileSync(join(root, "node/src/runtime-language.generated.ts"), ts)
writeFileSync(join(root, "tests/fixtures/runtime-language/vocabulary.json"), JSON.stringify({
  version: registry.version,
  public: registry.layers.public,
  host: registry.layers.runtime,
  kernel: registry.layers.kernel,
  provider: registry.layers.provider,
  primaryDomain: registry.primaryDomain,
  verbs: registry.verbs,
}, null, 2) + "\n")
