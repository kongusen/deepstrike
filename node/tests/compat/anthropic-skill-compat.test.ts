import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { loadSkill } from "../../src/skill.js"

/** spc_007-03 ①: spc_001-04's `loadSkill()` already satisfies "directly compatible with a
 *  `SKILL.md`-style directory" (Anthropic Agent Skills format — frontmatter + body). This is a
 *  no-new-code demonstration, not a reimplementation — same fixture shape spc_001-04's own test
 *  uses, framed here as "this is the Anthropic compatibility path" per spc_007 §3. */
describe("spc_007-03: Anthropic Skill compatibility (via spc_001-04's loadSkill)", () => {
  it("loads a SKILL.md-style directory into the public Skill shape, no conversion step needed", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-anthropic-skill-compat-"))
    await writeFile(
      join(dir, "researcher.md"),
      "---\nname: researcher\ndescription: digs up facts\n---\nBe thorough and cite sources.",
    )

    const skill = await loadSkill(dir, "researcher")

    expect(skill).not.toBeNull()
    expect(skill?.name).toBe("researcher")
    expect(skill?.description).toBe("digs up facts")
    expect(skill?.instructions).toBe("Be thorough and cite sources.")
  })
})
