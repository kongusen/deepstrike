import { createAgent } from "../src/index.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { SessionEvent, SessionLog } from "../src/runtime/session-log.js"
import type { MemoryRecord } from "../src/memory/protocols.js"

const scope = { tenant_id: "tenant-memory-boundary", namespace: "agent" }

function record(name: string): MemoryRecord {
  return {
    record_id: `record-${name}`,
    scope,
    name,
    kind: "project",
    content: "Use focused tests",
    description: "A host verified project fact",
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1,
    updated_at: 1,
    recall_count: 0,
    confidence: 1,
    links: [],
    pinned: false,
  }
}

describe("public Agent memory boundary", () => {
  it("routes host writes through validation and rejects denied writes before the store", async () => {
    const store = new InMemoryMemoryStore()
    const agent = createAgent({
      runtimeBinding: {
        runtimeOptions: { memoryPolicy: { maxNameLength: 3 } },
      },
      memoryStore: store,
      memoryScope: scope,
    })

    await expect(agent.remember({ name: "too-long", content: "should be denied" })).rejects.toThrow(/memory write denied/i)
    await expect(agent.recall("should be denied")).resolves.toEqual([])
  })

  it("preserves host provenance and records write and recall lifecycle events", async () => {
    const store = new InMemoryMemoryStore([record("project")])
    const baseLog = new InMemorySessionLog()
    const sessions = new Set<string>()
    const sessionLog: SessionLog = {
      append: async (sessionId, event) => { sessions.add(sessionId); return baseLog.append(sessionId, event) },
      read: (sessionId, fromSeq, primitiveFilter) => baseLog.read(sessionId, fromSeq, primitiveFilter),
      latestSeq: sessionId => baseLog.latestSeq(sessionId),
    }
    const recallCalls: string[] = []
    const recordRecall = store.recordRecall.bind(store)
    store.recordRecall = async (agentId, recalls) => {
      recallCalls.push(`${agentId}:${recalls.length}`)
      await recordRecall(agentId, recalls)
    }
    const agent = createAgent({
      runtimeBinding: { provider: new ReplayProvider([]), sessionLog, runtimeOptions: { memoryPolicy: { retrievalTopK: 1 } } },
      memoryStore: store,
      memoryScope: scope,
    })

    const saved = await agent.remember({ name: "preference", content: "Use focused tests" })
    const recalled = await agent.recall("focused tests")

    expect(saved.provenance).toMatchObject({ author: "host", trust: "user_asserted" })
    expect(recalled).toHaveLength(1)
    expect(recallCalls).toHaveLength(1)
    const entries = (await Promise.all([...sessions].map(id => baseLog.read(id)))).flat()
    const events = entries.map(entry => entry.event.kind)
    expect(events).toEqual(expect.arrayContaining(["memory_written", "memory_retrieval_result"]))
  })
})
