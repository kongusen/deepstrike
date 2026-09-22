import { mkdtemp, mkdir, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { DirectorySkillSource, InlineSkillSource, projectSkillRequirement, resolveSkillRevision, ResolverSkillSource } from "../src/skill.js"

test("inline and resolver sources share one source-independent Skill contract", async () => {
  const skill = { name: "research", instructions: "compare sources" }
  const context = { userId: "u1" }
  const inline = new InlineSkillSource([skill])
  const inlineRevision = await inline.resolve({ name: "research" }, context)
  await expect(inline.load(inlineRevision)).resolves.toMatchObject({ instructions: "compare sources" })
  const remote = new ResolverSkillSource(async ref => ({ descriptor: { name: ref.name, description: "" }, instructions: "compare sources", resources: { scripts: [], references: [], assets: [] } }), async () => [{ name: "research", description: "" }])
  const remoteRevision = await remote.resolve({ name: "research" }, context)
  await expect(remote.load(remoteRevision)).resolves.toMatchObject({ instructions: "compare sources" })
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

test("resolved revision preserves package identity without claiming kernel activation", () => {
  const resolved = resolveSkillRevision({
    descriptor: { name: "financial-report", description: "", version: "v1", digest: "sha256:x", allowedTools: ["read"] },
    instructions: "",
    resources: { scripts: [], references: [], assets: [] },
  }, { name: "financial-report", version: "v1" })
  expect(resolved).toMatchObject({ ref: { name: "financial-report", version: "v1" }, digest: "sha256:x", allowedTools: ["read"] })
  expect(resolved).not.toHaveProperty("activationId")
  expect(Object.isFrozen(resolved)).toBe(true)
})
