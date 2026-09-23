import { FileDynamicWorkflowStore } from "../src/workflow/dynamic-store.js"
import type { DynamicWorkflowScript } from "../src/workflow/dynamic.js"
import { lstat, symlink, mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

const script: DynamicWorkflowScript = {
  meta: { name: "audit", description: "Audit changed files", phases: ["audit"], sizeGuideline: "small" },
  source: "export const meta = { name: 'audit', description: 'Audit changed files' }\nreturn []",
}

describe("FileDynamicWorkflowStore", () => {
  it("round-trips validated artifacts and lists safe names", async () => {
    const root = await mkdtemp(join(tmpdir(), "dynamic-wf-store-"))
    try {
      const store = new FileDynamicWorkflowStore({ rootDir: root })
      expect(await store.save("audit", script)).toBe(join(root, "audit.json"))
      expect(await store.load("audit")).toEqual(script)
      expect(await store.list()).toEqual(["audit"])
      await expect(store.save("../escape", script)).rejects.toThrow(/invalid dynamic workflow name/)
      await expect(store.save("broken", { ...script, source: " " })).rejects.toThrow(/source must be non-empty/)
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  it("refuses symlinked stores and artifacts", async () => {
    const root = await mkdtemp(join(tmpdir(), "dynamic-wf-store-link-"))
    const target = await mkdtemp(join(tmpdir(), "dynamic-wf-store-target-"))
    try {
      const linkedRoot = join(root, "linked")
      await symlink(target, linkedRoot)
      await expect(new FileDynamicWorkflowStore({ rootDir: linkedRoot }).save("audit", script)).rejects.toThrow(/symlink/)

      const store = new FileDynamicWorkflowStore({ rootDir: root })
      await store.save("audit", script)
      const artifact = join(root, "audit.json")
      const artifactTarget = join(root, "artifact-target.json")
      await rm(artifact)
      await symlink(artifactTarget, artifact)
      await expect(store.load("audit")).rejects.toThrow(/symlink/)
      expect((await lstat(artifact)).isSymbolicLink()).toBe(true)
    } finally {
      await rm(root, { recursive: true, force: true })
      await rm(target, { recursive: true, force: true })
    }
  })
})

