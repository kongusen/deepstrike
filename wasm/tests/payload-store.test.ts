import { PayloadStore, type PayloadStorageDriver } from "../src/runtime/payload-store.js"

describe("WASM PayloadStore", () => {
  it("round-trips opaque locators and isolates sessions", async () => {
    const store = new PayloadStore()
    await store.persistPayload("session-a", "../../payload", "alpha")
    await store.persistPayload("session-b", "../../payload", "beta")

    expect(await store.loadPayload("session-a", "../../payload")).toBe("alpha")
    expect(await store.loadPayload("session-b", "../../payload")).toBe("beta")
    expect(await store.loadPayload("session-c", "../../payload")).toBeUndefined()
  })

  it("supports a host-provided storage driver", async () => {
    const values = new Map<string, string>()
    const driver: PayloadStorageDriver = {
      write: (key, value) => { values.set(key, value) },
      read: key => values.get(key),
      delete: key => { values.delete(key) },
      list: () => [...values.keys()],
    }
    const store = new PayloadStore({ driver })

    await store.persistPayload("session", "payload:1", "content")

    expect(values.size).toBe(1)
    expect([...values.keys()][0]).toMatch(/^payload\/[0-9a-f]{64}$/)
    expect(await store.loadPayload("session", "payload:1")).toBe("content")
  })

  it("propagates driver failures and cleans only payload keys", async () => {
    const values = new Map<string, { content: string; mtime: number }>([
      ["shared/config", { content: "keep", mtime: 0 }],
    ])
    const driver: PayloadStorageDriver = {
      write: (key, content) => { values.set(key, { content, mtime: 0 }) },
      read: key => {
        if (key.startsWith("payload/")) throw new Error("storage offline")
        return values.get(key)?.content
      },
      delete: key => { values.delete(key) },
      list: () => [...values.keys()],
      mtime: key => values.get(key)?.mtime ?? 0,
    }
    const store = new PayloadStore({ driver })

    await expect(store.loadPayload("session", "payload:1")).rejects.toThrow("storage offline")
    await store.cleanup(1)
    expect(values.has("shared/config")).toBe(true)
  })
})
