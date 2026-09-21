import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"
import { CANONICAL_PREFIX_ALLOWLIST } from "../src/canonical-prefix-allowlist.js"

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name)
    return entry.isDirectory() ? sourceFiles(path) : path.endsWith(".ts") ? [path] : []
  })
}

test("SPC-028-03 allowlist matches the shared fixture", () => {
  expect([...CANONICAL_PREFIX_ALLOWLIST]).toEqual(JSON.parse(readFileSync(
    join(process.cwd(), "../tests/fixtures/runtime-language/canonical-prefix-allowlist.json"), "utf8",
  )))
})

test("SPC-028-03 rejects undefined Canonical-prefixed names in source", () => {
  const allowed = new Set(CANONICAL_PREFIX_ALLOWLIST)
  const found = new Set<string>()
  for (const file of sourceFiles(join(process.cwd(), "src"))) {
    const source = readFileSync(file, "utf8")
    for (const match of source.matchAll(/\bCanonical[A-Z][A-Za-z0-9_]*/g)) found.add(match[0])
  }
  expect([...found].filter(name => !allowed.has(name))).toEqual([])
})
