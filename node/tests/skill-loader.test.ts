import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { scanSkillDir } from "../src/skills/loader.js"
import { skillMetadataToKernel } from "../src/runtime/kernel-step.js"

/**
 * P1-B B0: the `allowed_tools` pipe — frontmatter → SkillMetadata → kernel JSON. Pure plumbing,
 * no behavior change. Absent declarations stay byte-identical on the wire (additive).
 */
describe("P1-B B0: skill allowed_tools pipe", () => {
  it("parses allowed_tools frontmatter (comma + bracket forms) and forwards to the kernel", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-skill-loader-"))
    await writeFile(
      join(dir, "debug.md"),
      "---\nname: debug\ndescription: Debug helper\nallowed_tools: read, grep, bash\n---\nbody",
    )
    await writeFile(
      join(dir, "review.md"),
      "---\nname: review\ndescription: Reviewer\nallowed_tools: [git_diff, read]\n---\nbody",
    )
    await writeFile(
      join(dir, "plain.md"),
      "---\nname: plain\ndescription: No tools declared\n---\nbody",
    )

    const metas = await scanSkillDir(dir)
    const byName = Object.fromEntries(metas.map(m => [m.name, m]))

    expect(byName.debug.allowedTools).toEqual(["read", "grep", "bash"])
    expect(byName.review.allowedTools).toEqual(["git_diff", "read"])
    expect(byName.plain.allowedTools).toBeUndefined()

    // Forwarding: declared → present; absent → key omitted (wire unchanged for old skills).
    expect(skillMetadataToKernel(byName.debug).allowed_tools).toEqual(["read", "grep", "bash"])
    expect("allowed_tools" in skillMetadataToKernel(byName.plain)).toBe(false)
  })
})
