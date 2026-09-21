import { existsSync, readFileSync } from "node:fs"
import { join } from "node:path"

test("SPC-028-65 compatibility implementation and ProviderMessage alias are removed", () => {
  expect(existsSync(join(process.cwd(), "src/compat"))).toBe(false)
  expect(readFileSync(join(process.cwd(), "src/types.ts"), "utf8")).not.toMatch(/ProviderMessage/)
  expect(readFileSync(join(process.cwd(), "src/index.ts"), "utf8")).not.toMatch(/ProviderMessage/)
})
