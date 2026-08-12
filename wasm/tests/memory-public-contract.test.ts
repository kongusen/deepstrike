import { DurableMemory } from "../src/memory/durable.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"
import type { Memory, MemoryRecord, MemoryScope, MemoryStore } from "../src/memory/index.js"

const scope: MemoryScope = { tenant_id: "tenant-test", namespace: "research" }

function record(id: string, content: string): MemoryRecord {
  return {
    record_id: id,
    scope,
    name: id,
    kind: "project",
    content,
    description: content,
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1,
    updated_at: 1,
    recall_count: 0,
    confidence: 1,
    links: [],
    pinned: false,
  }
}

describe("spc_015-02: durable public Memory contract", () => {
  it("binds search/get/put/delete to one agent namespace", async () => {
    const store: MemoryStore = new InMemoryMemoryStore()
    const memory: Memory = new DurableMemory(store, "agent-a", scope)

    await memory.put(record("architecture", "kernel architecture notes"))
    expect((await memory.search("architecture", { topK: 1 })).map(value => value.record_id)).toEqual(["architecture"])
    expect((await memory.get("architecture"))?.content).toBe("kernel architecture notes")

    await memory.delete("architecture")
    expect(await memory.get("architecture")).toBeNull()
    expect(await memory.search("architecture")).toEqual([])
  })

  it("does not allow a bound Memory to read or write another namespace", async () => {
    const store: MemoryStore = new InMemoryMemoryStore()
    const memory: Memory = new DurableMemory(store, "agent-a", scope)
    const foreign = { ...record("foreign", "private note"), scope: { tenant_id: "tenant-test", namespace: "private" } }

    await expect(memory.put(foreign)).rejects.toThrow("scope")
    await store.put("agent-a", foreign)
    expect(await memory.get("foreign")).toBeNull()
    await memory.delete("foreign")
    expect((await store.get("agent-a", "foreign"))?.record_id).toBe("foreign")
  })
})
