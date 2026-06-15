import { FileWorkflowStore } from "../src/runtime/workflow-store.js"
import type { WorkflowSpec } from "../src/types/agent.js"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

describe("FileWorkflowStore", () => {
  it("round-trips a spec exactly, lists it, and rejects unsafe names", async () => {
    const root = await mkdtemp(join(tmpdir(), "wf-store-"))
    try {
      const store = new FileWorkflowStore({ rootDir: root })
      const spec: WorkflowSpec = {
        nodes: [
          { task: "explore", role: "explore", isolation: "read_only" },
          { task: "judge", role: "plan", dependsOn: [0], tournament: { entrants: ["x", "y"] }, tokenBudget: 10000 },
        ],
      }
      const path = await store.save("my-flow", spec)
      expect(path).toContain("my-flow.json")
      expect(await store.list()).toEqual(["my-flow"])

      const loaded = await store.load("my-flow")
      expect(loaded).toEqual(spec) // pure data ⇒ exact round-trip

      await expect(store.save("../evil", spec)).rejects.toThrow(/invalid/)
      await expect(store.load("does-not-exist")).rejects.toThrow()
    } finally {
      await rm(root, { recursive: true, force: true })
    }
  })

  it("returns [] for a store directory that does not exist yet", async () => {
    const store = new FileWorkflowStore({ rootDir: join(tmpdir(), "wf-store-missing-xyz-123") })
    expect(await store.list()).toEqual([])
  })
})
