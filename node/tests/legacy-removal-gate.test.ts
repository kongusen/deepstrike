import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"

test("SPC-028-65 compatibility implementation and ProviderMessage alias are removed", () => {
  expect(existsSync(join(process.cwd(), "src/compat"))).toBe(false)
  expect(readFileSync(join(process.cwd(), "src/types.ts"), "utf8")).not.toMatch(/ProviderMessage/)
  expect(readFileSync(join(process.cwd(), "src/index.ts"), "utf8")).not.toMatch(/ProviderMessage/)
})

test("formal Agent IR implementation and public projections are removed", () => {
  expect(existsSync(join(process.cwd(), "src", "agent-ir.ts"))).toBe(false)
  expect(readFileSync(join(process.cwd(), "src", "conformance.ts"), "utf8")).not.toMatch(/agent-ir|lowerAgent|normalizeAgent/)
  expect(readFileSync(join(process.cwd(), "src", "runtime", "public.ts"), "utf8")).not.toMatch(/projectAgent/)
  expect(readFileSync(join(process.cwd(), "src", "advanced", "public.ts"), "utf8")).not.toMatch(/projectAgent/)
})
