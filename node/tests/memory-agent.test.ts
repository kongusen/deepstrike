import { selectMemories, validateMemory } from "../src/memory/agent.js"
import type { MemoryQuery, MemoryRecord } from "../src/memory/protocols.js"

const scope = { tenant_id: "tenant-test", namespace: "memory-agent" }
const record = (name: string, description: string, updated_at: number): MemoryRecord => ({
  record_id: `record-${name}`, scope, name, kind: "project", content: description, description,
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at, recall_count: 0, confidence: 1, links: [], pinned: false,
})

const query: MemoryQuery = {
  scope,
  query: "oauth token expiry",
  top_k: 2,
  kinds: [],
}

describe("memory agent policy", () => {
  it("selects memory ids by deterministic lexical relevance and recency", async () => {
    const retrieval = await selectMemories(query, [
      record("db", "database migrations", 30),
      record("old-token", "oauth token handling", 10),
      record("new-token", "oauth token handling", 20),
    ])

    expect(retrieval.map(hit => hit.record.name)).toEqual(["new-token", "old-token"])
    expect(retrieval.every(hit => !/stub/i.test(hit.why))).toBe(true)
  })

  it("does not locally reject content using language-specific forbidden substrings", () => {
    const memory = record("architecture-note", "Evidence-backed project context", 1)
    memory.content = "架构: this is evidence from the current session"

    expect(validateMemory(memory)).toEqual({ valid: true })
  })
})
