import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"
import type { CurationResult, DreamStore, MemoryEntry } from "../../src/memory/protocols.js"

const provider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {},
}

describe("Phase-7 memory syscalls", () => {
  it("validates WriteMemory through the kernel and commits to DreamStore", async () => {
    let committed: CurationResult | null = null
    const dreamStore: DreamStore = {
      loadSessions: async () => [],
      loadMemories: async () => [],
      commit: async (_agentId, result) => {
        committed = result
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

    await runner.writeMemory({
      metadata: {
        name: "prefers-small-tests",
        description: "User prefers small focused tests",
        kind: "feedback",
        created_at: 1,
        updated_at: 1,
      },
      content: "User prefers focused unit tests for SDK behavior.",
    }, { sessionId: "memory-syscall" })

    expect(committed?.toAdd[0]?.text).toBe("User prefers focused unit tests for SDK behavior.")
    expect(committed?.toAdd[0]?.metadata).toMatchObject({ name: "prefers-small-tests", kind: "feedback" })

    const events = await sessionLog.read("memory-syscall")
    expect(events.some(e => e.event.kind === "memory_written")).toBe(true)
  })

  it("validates QueryMemory through the kernel and returns DreamStore hits", async () => {
    const hit: MemoryEntry = { text: "Use small focused tests.", score: 0.9, metadata: { name: "testing" } }
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-memory",
      dreamStore: {
        loadSessions: async () => [],
        loadMemories: async () => [],
        commit: async () => {},
        saveSession: async () => {},
        search: async (_agentId, query, topK) => query.includes("tests") && topK === 1 ? [hit] : [],
      },
    })

    const hits = await runner.queryMemory({
      current_context: "Need memory about tests",
      active_tools: [],
      already_surfaced: [],
      top_k: 1,
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
        loadSessions: async () => [],
        loadMemories: async () => [],
        commit: async () => {},
        saveSession: async () => {},
        search: async () => [],
      },
    })

    await runner.writeMemory({
      metadata: {
        name: "",
        description: "missing name",
        kind: "feedback",
        created_at: 1,
        updated_at: 1,
      },
      content: "invalid write",
    }, { sessionId: "memory-validation-fail" })

    const events = await sessionLog.read("memory-validation-fail")
    expect(events.some(e => e.event.kind === "memory_validation_failed")).toBe(true)
    expect(events.some(e => e.event.kind === "memory_written")).toBe(false)
  })
})
