import {
  DynamicWorkflowArtifactCatalog,
  FileDynamicWorkflowStore,
  createDynamicWorkflowArtifactCatalog,
  decodeDynamicWorkflowArtifact,
  encodeDynamicWorkflowArtifact,
} from "../src/workflow/dynamic-store.js"
import { createDynamicWorkflowArtifact } from "../src/workflow/dynamic.js"
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
      await expect(store.save("broken", { ...script, meta: { ...script.meta, name: "broken" }, source: " " })).rejects.toThrow(/source must be non-empty/)
      await expect(store.save("other", { ...script, meta: { ...script.meta, name: "different" } })).rejects.toThrow(/does not match artifact name/)
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

  it("discovers across roots and distributes digest-checked bundles", async () => {
    const firstRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-store-first-"))
    const secondRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-store-second-"))
    try {
      const first = new FileDynamicWorkflowStore({ rootDir: firstRoot })
      const second = new FileDynamicWorkflowStore({ rootDir: secondRoot })
      await first.save("audit", script)
      const catalog = new DynamicWorkflowArtifactCatalog([first, second])
      const found = await catalog.discover()
      expect(found).toHaveLength(1)
      expect(found[0]).toMatchObject({ name: "audit", origin: "file-store" })
      const artifact = await catalog.load("audit", found[0].digest)
      const bundle = encodeDynamicWorkflowArtifact(artifact)
      expect(decodeDynamicWorkflowArtifact(bundle)).toMatchObject({ name: "audit", digest: found[0].digest })
      await catalog.distribute("audit", second)
      await expect(second.load("audit")).resolves.toEqual(script)
      expect(() => decodeDynamicWorkflowArtifact(bundle.replace(found[0].digest, "0".repeat(64)))).toThrow(/digest mismatch/)
      expect(createDynamicWorkflowArtifact(script).digest).toBe(found[0].digest)
      expect(() => decodeDynamicWorkflowArtifact(JSON.stringify({
        version: 1,
        artifact: { name: "audit", digest: found[0].digest, script: { meta: { name: "audit" }, source: "return 1" } },
      }))).toThrow(/description must be a non-empty string/)
    } finally {
      await rm(firstRoot, { recursive: true, force: true })
      await rm(secondRoot, { recursive: true, force: true })
    }
  })

  it("uses project artifacts before user/plugin/package stores and keeps read-only sources immutable", async () => {
    const projectRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-project-"))
    const userRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-user-"))
    const pluginRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-plugin-"))
    const packageRoot = await mkdtemp(join(tmpdir(), "dynamic-wf-package-"))
    try {
      const project = new FileDynamicWorkflowStore({ rootDir: projectRoot })
      const user = new FileDynamicWorkflowStore({ rootDir: userRoot })
      await project.save("shared", { ...script, meta: { ...script.meta, name: "shared" } })
      await user.save("user-only", { ...script, meta: { ...script.meta, name: "user-only" } })
      await new FileDynamicWorkflowStore({ rootDir: pluginRoot }).save("plugin-only", { ...script, meta: { ...script.meta, name: "plugin-only" } })
      await new FileDynamicWorkflowStore({ rootDir: packageRoot }).save("package-only", { ...script, meta: { ...script.meta, name: "package-only" } })

      const catalog = createDynamicWorkflowArtifactCatalog({ projectRoot, userRoot, pluginRoots: [pluginRoot], packageRoots: [packageRoot] })
      await expect(catalog.discover()).resolves.toEqual([
        expect.objectContaining({ name: "package-only", scope: "package" }),
        expect.objectContaining({ name: "plugin-only", scope: "plugin" }),
        expect.objectContaining({ name: "shared", scope: "project" }),
        expect.objectContaining({ name: "user-only", scope: "user" }),
      ])
      await expect(catalog.load("shared")).resolves.toMatchObject({ origin: "project" })
      await expect(catalog.load("plugin-only")).resolves.toMatchObject({ origin: `plugin:${pluginRoot}` })
      const pluginStore = new FileDynamicWorkflowStore({ rootDir: pluginRoot, scope: "plugin", origin: "plugin", readOnly: true })
      await expect(pluginStore.save("blocked", script)).rejects.toThrow(/read-only/)
    } finally {
      await Promise.all([rm(projectRoot, { recursive: true, force: true }), rm(userRoot, { recursive: true, force: true }), rm(pluginRoot, { recursive: true, force: true }), rm(packageRoot, { recursive: true, force: true })])
    }
  })
})
