import { mkdtemp, mkdir, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { activateSkill, DirectorySkillCatalog, DirectorySkillSource, InlineSkillCatalog, projectSkillRequirement, ResolverSkillCatalog } from "../src/skill.js"

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

test("directory source loads Claude-compatible package anatomy with lazy resources", async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), "deepstrike-skill-package-"))
  const dir = path.join(root, "default", "u1", "financial-report")
  await mkdir(path.join(dir, "scripts"), { recursive: true })
  await mkdir(path.join(dir, "references"), { recursive: true })
  await mkdir(path.join(dir, "assets"), { recursive: true })
  await writeFile(path.join(dir, "SKILL.md"), "---\nname: financial-report\ndescription: Report helper\nversion: v1\n---\nFollow the report workflow")
  await writeFile(path.join(dir, "scripts", "calculate.py"), "print(1)")
  await writeFile(path.join(dir, "references", "policy.md"), "policy")
  await writeFile(path.join(dir, "assets", "template.txt"), "template")
  const source = new DirectorySkillSource(root)
  const pkg = await source.load({ name: "financial-report", version: "v1" }, { userId: "u1" })
  expect(pkg.instructions).toContain("report workflow")
  expect(pkg.resources.scripts.map(resource => resource.path)).toEqual(["scripts/calculate.py"])
  await expect(source.readResource(pkg, pkg.resources.references[0]!)).resolves.toEqual(new TextEncoder().encode("policy"))
})

test("skill declarations accept source-independent string and object references", () => {
  expect(projectSkillRequirement("financial-report")).toEqual({ name: "financial-report" })
  expect(projectSkillRequirement({ name: "financial-report", version: "v1" })).toEqual({ name: "financial-report", version: "v1" })
})

test("activation preserves package identity and narrows declared tools", () => {
  const activated = activateSkill({
    descriptor: { name: "financial-report", description: "", version: "v1", digest: "sha256:x", allowedTools: ["read"] },
    instructions: "",
    resources: { scripts: [], references: [], assets: [] },
    root: "/tmp/skill",
  }, { name: "financial-report", version: "v1" })
  expect(activated).toMatchObject({ ref: { name: "financial-report", version: "v1" }, digest: "sha256:x", allowedTools: ["read"] })
  expect(Object.isFrozen(activated)).toBe(true)
})
