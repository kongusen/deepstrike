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
})
