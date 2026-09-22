import { mkdtemp, mkdir, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { DirectorySkillCatalog, InlineSkillCatalog, ResolverSkillCatalog } from "../src/skill.js"

test("inline and resolver catalogs share one source-independent Skill contract", async () => {
  const skill = { name: "research", instructions: "compare sources" }
  const context = { userId: "u1" }
  await expect(new InlineSkillCatalog([skill]).resolve({ name: "research" }, context)).resolves.toEqual(skill)
  await expect(new ResolverSkillCatalog(async ref => ({ ...skill, name: ref.name }), async () => [{ name: "research" }]).resolve({ name: "research" }, context)).resolves.toEqual(skill)
})

test("directory catalog is user-scoped and rejects unsafe scope segments", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "deepstrike-skills-"))
  const dir = path.join(root, "default", "u1")
  await mkdir(dir, { recursive: true })
  await writeFile(path.join(dir, "research.md"), "---\ndescription: Compare sources\n---\nUse citations")
  const catalog = new DirectorySkillCatalog(root)
  await expect(catalog.resolve({ name: "research" }, { userId: "u1" })).resolves.toMatchObject({ name: "research", instructions: "Use citations" })
  await expect(catalog.list({ userId: "../other" })).rejects.toThrow("unsafe segment")
})
