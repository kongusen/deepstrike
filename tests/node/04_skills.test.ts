/**
 * 04_skills.test.ts — Skill file loading + agent skillDir
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { scanSkillDir, readSkillFile } from "@deepstrike/sdk"
import { makeAgent, SKILL_DIR } from "./helpers.js"

describe("scanSkillDir()", () => {
  it("returns metadata for all .md files", async () => {
    const metas = await scanSkillDir(SKILL_DIR)
    assert.ok(metas.length >= 2, `got ${metas.length} skills`)
    const names = metas.map(m => m.name)
    assert.ok(names.includes("summarize"))
    assert.ok(names.includes("count_words"))
  })

  it("each entry has name and description", async () => {
    for (const m of await scanSkillDir(SKILL_DIR)) {
      assert.ok(m.name.length > 0)
      assert.ok(m.description.length > 0)
    }
  })

  it("parses optional frontmatter fields", async () => {
    const metas = await scanSkillDir(SKILL_DIR)
    const s = metas.find(m => m.name === "summarize")!
    assert.ok(s.whenToUse && s.whenToUse.length > 0)
    assert.ok(s.effort !== undefined)
  })

  it("returns [] for non-existent directory", async () => {
    assert.deepEqual(await scanSkillDir("/tmp/no-such-skills-xyz"), [])
  })
})

describe("readSkillFile()", () => {
  it("returns content for a known skill", async () => {
    const content = await readSkillFile(SKILL_DIR, "summarize")
    assert.ok(content !== null)
    assert.ok(content!.startsWith("---"))
  })

  it("returns null for unknown skill", async () => {
    assert.equal(await readSkillFile(SKILL_DIR, "does_not_exist"), null)
  })
})

describe("Agent with skillDir (real API)", () => {
  it("agent produces a summary when directed to use the skill", { timeout: 90_000 }, async () => {
    const agent = makeAgent({ skillDir: SKILL_DIR })
    const result = await agent.run(
      "Use the summarize skill to summarize: " +
      "'DeepStrike is a Rust-based AI agent framework with a pure-computation kernel " +
      "and bindings for Node.js, Python, and Rust.'",
    )
    assert.ok(result.length > 0)
  })
})
