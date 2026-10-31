import { InMemoryMemoryLifecycleStore } from "../src/memory/lifecycle-store.js"
import { MemoryWriter } from "../src/memory/writer.js"
import type { MemoryCandidateSink } from "../src/memory/candidate-sink.js"
import type { MemoryRecord, MemoryStore } from "../src/memory/protocols.js"

const scope = { tenant_id: "t", namespace: "n" }

function record(id: string): MemoryRecord {
  return {
    record_id: id, scope, name: "brief-style", kind: "project", content: "prefers numbered sections",
    description: "", provenance: { author: "model", trust: "untrusted", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: 0, confidence: 1, links: [], pinned: false,
  }
}

function countingStore(): MemoryStore & { puts: string[] } {
  const puts: string[] = []
  return {
    puts,
    async put(_agent: string, memory: MemoryRecord) { puts.push(memory.record_id) },
    async get() { return null },
    async delete() {},
    async search() { return [] },
    async saveSession() {},
    async recordRecall() {},
    async setPinned() {},
  } as MemoryStore & { puts: string[] }
}

/** The host's side of a memory write, outside the runner. */
describe("MemoryWriter", () => {
  it("writes once per write id within the lease and refuses past its budget", async () => {
    const store = countingStore()
    const lifecycle = new InMemoryMemoryLifecycleStore()
    await lifecycle.issueLease({ lease_id: "l", agent_id: "agent", scope, issued_at: 0, expires_at: 10_000, max_writes: 1, writes_used: 0 })
    const writer = new MemoryWriter({ store, lifecycle, now: () => 5 })
    const receipt = await writer.persist("agent", record("memory:1"), "write-1")
    expect(receipt).toEqual({ record_ref: "memory:1", digest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/) })
    // A redelivered write spends the budget once.
    await writer.persist("agent", record("memory:1"), "write-1")
    await expect(writer.persist("agent", record("memory:2"), "write-2")).rejects.toThrow("memory lease rejected")
    expect(store.puts).toEqual(["memory:1", "memory:1"])
  })

  it("requires a store for a direct write and a sink for a candidate", async () => {
    await expect(new MemoryWriter({}).persist("agent", record("memory:1"), "w")).rejects.toThrow("requires memoryStore")
    await expect(new MemoryWriter({}).propose("agent", { candidate_id: "c", source: "model_proposal", record: record("memory:1") }))
      .rejects.toThrow("memoryCandidateSink")
  })

  it("refuses a sink answer that does not name the candidate or breaks the receipt contract", async () => {
    const answering = (answer: unknown): MemoryCandidateSink => ({ async submit() { return answer as never } })
    const candidate = { candidate_id: "c", source: "model_proposal" as const, record: record("memory:1") }
    const propose = (answer: unknown) => new MemoryWriter({ sink: answering(answer) }).propose("agent", candidate)
    await expect(propose({ status: "pending", candidate_id: "c", receipt_id: "r" })).resolves.toMatchObject({ status: "pending" })
    await expect(propose({ status: "pending", candidate_id: "other", receipt_id: "r" })).rejects.toThrow("without a receipt for c")
    await expect(propose({ status: "accepted", candidate_id: "c", receipt_id: "r" })).rejects.toThrow("record_ref/digest")
    await expect(propose({ status: "rejected", candidate_id: "c", receipt_id: "r" })).rejects.toThrow("reason_code")
    await expect(propose({ status: "maybe", candidate_id: "c", receipt_id: "r" })).rejects.toThrow("unknown status")
  })
})
