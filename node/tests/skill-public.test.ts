import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { loadSkill } from "../src/skill.js"

describe("spc_001-04: Skill public type + loadSkill()", () => {
  it("loads name/description/instructions from an existing skill file via the current loader", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-skill-public-"))
    await writeFile(
      join(dir, "researcher.md"),
      "---\nname: researcher\ndescription: digs up facts\n---\nBe thorough and cite sources.",
    )

    const skill = await loadSkill(dir, "researcher")

    expect(skill.name).toBe("researcher")
    expect(skill.description).toBe("digs up facts")
    expect(skill.instructions).toBe("Be thorough and cite sources.")
  })

  it("returns null for a skill that does not exist in the directory", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-skill-public-"))
    const skill = await loadSkill(dir, "ghost")
    expect(skill).toBeNull()
  })
})
