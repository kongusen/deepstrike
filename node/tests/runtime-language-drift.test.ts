import { readFileSync } from "node:fs"
import { join } from "node:path"

test("SPC-028-04 rejects the retired countTokens dispatch comment", () => {
  const source = readFileSync(join(process.cwd(), "src/types.ts"), "utf8")
  expect(source).not.toContain("Not currently invoked by any dispatch loop")
  expect(source).toContain("host runner invokes this capability during provider request")
})

test("SPC-028-04 keeps the runner's measurement path documented by executable symbols", () => {
  const runner = readFileSync(join(process.cwd(), "src/runtime/runner.ts"), "utf8")
  expect(runner).toContain("preparedRequest.countTokens")
  expect(runner).toContain("recordPromptMeasurement")
})
