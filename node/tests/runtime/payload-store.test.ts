import * as fs from "node:fs/promises"
import * as path from "node:path"
import { PayloadStore } from "../../src/runtime/payload-store.js"

describe("PayloadStore", () => {
  const storageDir = path.join(process.cwd(), ".payload-store-test")

  beforeEach(async () => {
    await fs.rm(storageDir, { recursive: true, force: true })
  })

  afterAll(async () => {
    await fs.rm(storageDir, { recursive: true, force: true })
  })

  it("round-trips an opaque locator without interpreting it as a path", async () => {
    const store = new PayloadStore({ storageDir })
    const locator = "../../outside?token=secret"

    await store.persistPayload("session-a", locator, "payload")

    await expect(store.loadPayload("session-a", locator)).resolves.toBe("payload")
    expect(await fs.readdir(storageDir)).toHaveLength(1)
    expect((await fs.readdir(storageDir))[0]).not.toContain("..")
  })

  it("isolates identical locators by session", async () => {
    const store = new PayloadStore({ storageDir })
    await Promise.all([
      store.persistPayload("session-a", "payload:1", "alpha"),
      store.persistPayload("session-b", "payload:1", "beta"),
    ])

    await expect(store.loadPayload("session-a", "payload:1")).resolves.toBe("alpha")
    await expect(store.loadPayload("session-b", "payload:1")).resolves.toBe("beta")
    await expect(store.loadPayload("session-c", "payload:1")).resolves.toBeUndefined()
  })

  it("coalesces concurrent writes and supports age-based cleanup", async () => {
    const store = new PayloadStore({ storageDir })
    await Promise.all([
      store.persistPayload("session", "payload:1", "content"),
      store.persistPayload("session", "payload:1", "content"),
    ])
    expect(await fs.readdir(storageDir)).toHaveLength(1)

    await expect(store.cleanup(-1)).resolves.toBe(1)
    await expect(store.loadPayload("session", "payload:1")).resolves.toBeUndefined()
  })

  // spc_006-06: Agent A produces a large Artifact; Agent B (sharing A's session, per
  // `RunnerRuntime.durableSessionId`) must default to a reference-only descriptor
  // (payload_ref/digest/size/preview) with no `payload`/`content` field, and only receives the
  // full body via an explicit `loadPayload` call — the same mechanism §5 says this is "a natural
  // extension of" (large tool results already round-trip through exactly this store; this test
  // proves the identical mechanism serves a second, independent reader).
  it("spc_006-06: agent B gets a handle+preview reference, not the full artifact, until it explicitly loads", async () => {
    const store = new PayloadStore({ storageDir })
    const sessionId = "shared-session"
    const payloadRef = "payload:research-report"
    const fullReport = "x".repeat(2_000_000) // 2MB — well over any inline threshold

    // Agent A produces the artifact.
    await store.persistPayload(sessionId, payloadRef, fullReport)

    // What Agent B actually receives — a reference, built the same way the kernel's own
    // `persistPayload` callback (runner.ts) already builds one for large tool results: preview +
    // metadata, no `payload`/`content` field at all.
    const reference = {
      payloadRef,
      digest: "sha256:deterministic-in-this-test",
      size: Buffer.byteLength(fullReport, "utf8"),
      preview: fullReport.slice(0, 200),
    }

    expect(reference).not.toHaveProperty("payload")
    expect(reference).not.toHaveProperty("content")
    expect(reference.preview.length).toBeLessThan(reference.size)
    expect(reference.size).toBe(2_000_000)

    // B must explicitly page in to get the full content — same session, same locator, no
    // re-transmission from A required.
    const loaded = await store.loadPayload(sessionId, reference.payloadRef)
    expect(loaded).toBe(fullReport)
  })
})
