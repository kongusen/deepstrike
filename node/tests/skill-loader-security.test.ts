import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { readSkillFile } from "../src/skills/loader.js"

describe("skill loader boundary", () => {
  let root: string

  beforeEach(async () => {
    root = await mkdtemp(join(tmpdir(), "ds-skill-loader-"))
  })

  afterEach(async () => {
    await rm(root, { recursive: true, force: true })
  })

  it("rejects skill names that escape the configured directory", async () => {
    const skillDir = join(root, "skills")
    await mkdir(skillDir)
    await writeFile(join(root, "secret.md"), "must not be readable", "utf8")

    await expect(readSkillFile(skillDir, "../secret")).rejects.toThrow("invalid skill name")
  })
})
