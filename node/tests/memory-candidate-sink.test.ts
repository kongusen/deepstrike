import {
  InMemoryMemoryCandidateSink,
  memoryCandidateId,
  type MemoryCandidate,
} from "../src/memory/candidate-sink.js"
import type { MemoryRecord } from "../src/memory/protocols.js"

/** M2 · the candidate sink contract (spec §2.3), pinned on the in-process implementation. */

const scope = { tenant_id: "t", namespace: "n" }

function record(content: string, overrides: Partial<MemoryRecord> = {}): MemoryRecord {
  return {
    record_id: "memory:any", scope, name: "brief-style", kind: "project", content, description: "",
    provenance: { author: "extraction", trust: "untrusted", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.5, links: [], pinned: false,
    ...overrides,
  }
}

function candidate(id: string, content: string, overrides: Partial<MemoryRecord> = {}): MemoryCandidate {
  return { candidate_id: id, source: "session_extraction", record: record(content, overrides) }
}

describe("memory candidate sink (M2)", () => {
  it("holds a new candidate as pending with a receipt the sink minted", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const receipt = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    expect(receipt).toEqual({ status: "pending", candidate_id: "cand-1", receipt_id: expect.any(String) })
    expect(sink.candidates("agent")).toHaveLength(1)
  })

  it("answers a redelivered candidate with its original receipt and stores nothing new (S04)", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const first = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    // A retry may mint a fresh store id and timestamps; the proposal itself is unchanged.
    const again = await sink.submit("agent", candidate("cand-1", "prefers numbered sections", {
      record_id: "memory:other", created_at: 99, updated_at: 99,
    }))
    expect(again).toEqual(first)
    expect(sink.candidates()).toHaveLength(1)
  })

  it("rejects the same candidate id with a different payload and keeps what it holds", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const first = await sink.submit("agent", candidate("cand-1", "prefers numbered sections"))
    const conflict = await sink.submit("agent", candidate("cand-1", "prefers prose"))
    expect(conflict).toMatchObject({ status: "rejected", candidate_id: "cand-1", reason_code: "candidate_conflict" })
    expect(conflict.receipt_id).not.toBe(first.receipt_id)
    expect(sink.candidates().map(held => held.record.content)).toEqual(["prefers numbered sections"])
  })

  it("derives candidate ids from the write's identity, never at random", () => {
    expect(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
      .toBe(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
    expect(memoryCandidateId(["child_request", "op-1", "effect-3", "0"]))
      .not.toBe(memoryCandidateId(["child_request", "op-1", "effect-4", "0"]))
    // Part boundaries are part of the identity: ["a", "bc"] and ["ab", "c"] differ.
    expect(memoryCandidateId(["a", "bc"])).not.toBe(memoryCandidateId(["ab", "c"]))
  })
})

it("isolates an identical caller-supplied ID by agent", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("same-id", "proposal")
  const a = await sink.submit("a", c)
  const b = await sink.submit("b", c)
  expect(a.receipt_id).not.toBe(b.receipt_id)
  expect(sink.candidates("a")).toHaveLength(1)
  expect(sink.candidates("b")).toHaveLength(1)
  expect(await sink.submit("a", c)).toEqual(a)
})

it("rejects changed source and review metadata without replacing the first proposal", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("c", "proposal")
  const original = await sink.submit("agent", c)
  const alternatives: MemoryCandidate[] = [
    { ...c, source: "child_request" },
    { ...c, record: { ...c.record, provenance: { ...c.record.provenance, trust: "host_verified" } } },
    { ...c, record: { ...c.record, provenance: { ...c.record.provenance, evidence_refs: ["new-evidence"] } } },
    { ...c, record: { ...c.record, confidence: 0.9 } },
    { ...c, record: { ...c.record, pinned: true } },
    { ...c, record: { ...c.record, links: ["related"] } },
    { ...c, record: { ...c.record, ttl_days: 7 } },
  ]
  for (const other of alternatives) {
    expect(await sink.submit("agent", other)).toMatchObject({ status: "rejected", reason_code: "candidate_conflict" })
  }
  expect(await sink.submit("agent", c)).toEqual(original)
  expect(sink.candidates()).toEqual([c])
})

it("snapshots input, output and receipt so callers cannot mutate held state", async () => {
  const sink = new InMemoryMemoryCandidateSink()
  const c = candidate("c", "proposal")
  const receipt = await sink.submit("agent", c)
  const original = { ...receipt }
  c.record.content = "tampered"
  c.record.provenance.evidence_refs.push("tampered")
  receipt.receipt_id = "tampered"
  sink.candidates()[0].record.links.push("tampered")
  expect(await sink.submit("agent", candidate("c", "proposal"))).toEqual(original)
  expect(sink.candidates()[0].record.content).toBe("proposal")
  expect(sink.candidates()[0].record.links).toEqual([])
})
