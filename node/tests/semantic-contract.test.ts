import { readFileSync } from "node:fs"
import { join } from "node:path"
import { RUNTIME_VOCABULARY } from "../src/runtime-language.js"
import { RUNTIME_OBJECT_CLASSIFICATIONS } from "../src/runtime-classification.js"

test("SPC-028-35/54 shared semantic contract is internally consistent", () => {
  const contract = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/runtime-language/semantic-contract.json"), "utf8")) as { version: string; authorities: Record<string, string> }
  expect(contract.version).toBe(RUNTIME_VOCABULARY.version)
  for (const [name, authority] of Object.entries(contract.authorities)) {
    expect(RUNTIME_OBJECT_CLASSIFICATIONS[name as keyof typeof RUNTIME_OBJECT_CLASSIFICATIONS]?.authority).toBe(authority)
  }
})
