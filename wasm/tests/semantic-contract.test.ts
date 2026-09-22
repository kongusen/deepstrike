import { readFileSync } from "node:fs"
import { join } from "node:path"

test("SPC-028-54 WASM consumes the shared semantic contract fixture", () => {
  const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/runtime-language/semantic-contract.json"), "utf8")) as { version: string; public: string[] }
  expect(fixture.version).toBe("0.2.73")
  expect(fixture.public).toContain("Agent")
})
