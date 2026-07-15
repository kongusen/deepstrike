import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"
import type { DreamStore, MemoryRecall, MemoryRecord } from "../../src/memory/protocols.js"

const scope = { tenant_id: "agent-memory", namespace: "runtime-tests" }
const memory = (name: string, content: string): MemoryRecord => ({
  record_id: `record-${name || "invalid"}`, scope, name, kind: "feedback", content,
  description: "User prefers small focused tests",
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
})

const provider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {},
}

describe("Phase-7 memory syscalls", () => {
  it("validates WriteMemory through the kernel and upserts to DreamStore", async () => {
    let committed: MemoryRecord | null = null
    const dreamStore: DreamStore = {
      upsert: async (_agentId, record) => {
        committed = record
      },
      saveSession: async () => {},
      search: async () => [],
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-memory",
      dreamStore,
    })

    await runner.writeMemory(memory(
      "prefers-small-tests",
      "User prefers focused unit tests for SDK behavior.",
    ), { sessionId: "memory-syscall" })

    expect(committed?.content).toBe("User prefers focused unit tests for SDK behavior.")
    expect(committed).toMatchObject({ name: "prefers-small-tests", kind: "feedback", scope })

    const events = await sessionLog.read("memory-syscall")
    expect(events.some(e => e.event.kind === "memory_written")).toBe(true)
  })

  it("validates QueryMemory through the kernel and returns DreamStore hits", async () => {
    const hit: MemoryRecall = {
      record: memory("testing", "Use small focused tests."),
      score: 0.9,
      why: "lexical match",
    }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-memory",
      dreamStore: {
        upsert: async () => {},
        saveSession: async () => {},
        search: async (_agentId, query) => query.query.includes("tests") && query.top_k === 1 ? [hit] : [],
      },
    })

    const hits = await runner.queryMemory({
      scope,
      query: "Need memory about tests",
      top_k: 1,
      kinds: [],
    }, { sessionId: "memory-query-syscall" })

    expect(hits).toEqual([hit])
    const events = await sessionLog.read("memory-query-syscall")
    expect(events.some(e => e.event.kind === "memory_queried")).toBe(true)
    expect(events.some(e => e.event.kind === "memory_retrieval_result")).toBe(true)
  })

  it("logs memory_validation_failed when kernel rejects a write", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-memory",
      dreamStore: {
        upsert: async () => {},
        saveSession: async () => {},
        search: async () => [],
      },
    })

    const invalid = memory("", "invalid write")
    invalid.description = "missing name"
    await runner.writeMemory(invalid, { sessionId: "memory-validation-fail" })

    const events = await sessionLog.read("memory-validation-fail")
    expect(events.some(e => e.event.kind === "memory_validation_failed")).toBe(true)
    expect(events.some(e => e.event.kind === "memory_written")).toBe(false)
  })
})
