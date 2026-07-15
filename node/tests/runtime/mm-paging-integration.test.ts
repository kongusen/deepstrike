import { createRunner, tool } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import type { ArchiveStore } from "../../src/runtime/archive.js"
import type { DreamStore, MemoryEntry } from "../../src/memory/protocols.js"
import type { InMemorySessionLog } from "../../src/runtime/session-log.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../../src/types.js"

const AGENT_ID = "agent-mm-paging"
const RECALL_MARKER = "LONGTERM_FACT_AFTER_COMPRESS"

class InMemoryArchiveStore implements ArchiveStore {
  private readonly blobs = new Map<string, Message[]>()

  async write(sessionId: string, seq: number, messages: Message[]): Promise<string> {
    const ref = `${sessionId}@${seq}`
    this.blobs.set(ref, messages)
    return ref
  }

  async read(archiveRef: string): Promise<Message[]> {
    return this.blobs.get(archiveRef) ?? []
  }
}

function pagingDreamStore(): DreamStore {
  return {
    loadSessions: async () => [],
    loadMemories: async () => [],
    commit: async () => {},
    saveSession: async () => {},
    search: async (_agentId, query) => {
      if (query.toLowerCase().includes("archived")) {
        return [{ text: RECALL_MARKER, score: 0.97, metadata: null } satisfies MemoryEntry]
      }
      return []
    },
  }
}

async function seedWakeSession(
  log: InMemorySessionLog,
  sourceId: string,
  destId: string,
): Promise<void> {
  const events = await log.read(sourceId)
  for (const { event } of events) {
    if (event.kind === "run_terminal") continue
    await log.append(destId, event)
  }
}

describe("long-session memory paging integration", () => {
  it("compress → page_out → archive_ref, then a memory tool call's result flows through history (not a permanent knowledge pin)", async () => {
    let providerCalls = 0
    let sawRecallInContext = false

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        providerCalls += 1
        // Strict dynamic context control: a memory-tool hit is single-use retrieval content, not a
        // stable skill — it must appear via the ordinary history/turns path (where it can later
        // decay with the compression pyramid), NOT get pinned into the permanent systemKnowledge slot.
        if (JSON.stringify(context.turns).includes(RECALL_MARKER)) {
          sawRecallInContext = true
        }
        expect(context.systemKnowledge ?? "").not.toContain(RECALL_MARKER)
        if (providerCalls <= 9) {
          yield { type: "tool_call", id: `bulk${providerCalls}`, name: "bulk", arguments: {} }
          return
        }
        if (providerCalls === 10) {
          yield {
            type: "tool_call",
            id: "mem1",
            name: "memory",
            arguments: { query: "archived session facts", top_k: 3 },
          }
          return
        }
        yield { type: "text_delta", delta: "recalled" }
      },
    }

    const archiveStore = new InMemoryArchiveStore()
    const { runner, sessionLog } = createRunner(
      provider,
      [
        tool("bulk", "bulk", { type: "object", properties: {} }, () => "z".repeat(240)),
      ],
      {
        maxTokens: 480,
        maxTurns: 30,
        agentId: AGENT_ID,
        dreamStore: pagingDreamStore(),
        compressionStore: archiveStore,
        // The script deliberately repeats an identical `bulk()` call 9 turns in a row to force
        // compression/paging — incidental to the repeat fuse's intent, so disabled for this test.
        repeatFuse: false,
      },
    )

    const sessionId = "paging-one-shot"
    const text = await collectText(runner.run({ sessionId, goal: "fill context then recall memory" }))
    expect(text).toBe("recalled")

    const events = await sessionLog.read(sessionId)
    expect(events.some(e => e.event.kind === "compressed")).toBe(true)
    expect(events.some(e => e.event.kind === "page_out")).toBe(true)
    // The live memory-tool-call page-in side channel was retired: no `page_in` event should fire
    // for an ordinary tool call anymore — its result travels through the normal tool_result path.
    expect(events.some(e => e.event.kind === "page_in")).toBe(false)

    const pageOut = events.find(e => e.event.kind === "page_out")
    expect(pageOut).toBeDefined()
    expect(pageOut).toBeDefined()
    expect((pageOut!.event as { message_count?: number }).message_count ?? 0).toBeGreaterThan(0)

    const archiveRef = (pageOut!.event as { archive_ref?: string }).archive_ref
    expect(archiveRef).toBeTruthy()
    const archived = await archiveStore.read(archiveRef!)
    expect(archived.length).toBeGreaterThan(0)

    expect(sawRecallInContext).toBe(true)
  })

  it("wake after compression replays history; a memory tool call on wake still flows through history, not knowledge", async () => {
    let compressCalls = 0
    let wakeStreamCalls = 0
    let sawRecallOnWake = false

    const compressProvider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        compressCalls += 1
        if (compressCalls <= 9) {
          yield { type: "tool_call", id: `bulk${compressCalls}`, name: "bulk", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "paused" }
      },
    }

    const archiveStore = new InMemoryArchiveStore()
    const dreamStore = pagingDreamStore()
    const sharedLog = createRunner(
      compressProvider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "y".repeat(240))],
      {
        maxTokens: 480,
        maxTurns: 30,
        agentId: AGENT_ID,
        dreamStore,
        compressionStore: archiveStore,
      },
    ).sessionLog

    const compressRunner = createRunner(
      compressProvider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "y".repeat(240))],
      {
        maxTokens: 480,
        maxTurns: 30,
        agentId: AGENT_ID,
        dreamStore,
        compressionStore: archiveStore,
        sessionLog: sharedLog,
        // The script deliberately repeats an identical `bulk()` call 9 turns in a row to force
        // compression/paging — incidental to the repeat fuse's intent, so disabled for this test.
        repeatFuse: false,
      },
    ).runner

    const compressSession = "paging-compress"
    await collectText(compressRunner.run({ sessionId: compressSession, goal: "fill until compact" }))

    const afterCompress = await sharedLog.read(compressSession)
    expect(afterCompress.some(e => e.event.kind === "compressed")).toBe(true)
    expect(afterCompress.some(e => e.event.kind === "page_out")).toBe(true)

    const wakeSession = "paging-wake"
    await seedWakeSession(sharedLog, compressSession, wakeSession)

    const wakeProvider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        wakeStreamCalls += 1
        if (JSON.stringify(context.turns).includes(RECALL_MARKER)) {
          sawRecallOnWake = true
        }
        if (wakeStreamCalls === 1) {
          yield {
            type: "tool_call",
            id: "mem_wake",
            name: "memory",
            arguments: { query: "archived session facts", top_k: 3 },
          }
          return
        }
        yield { type: "text_delta", delta: "woke" }
      },
    }

    const wakeRunner = createRunner(
      wakeProvider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "ok")],
      {
        maxTokens: 8192,
        maxTurns: 10,
        agentId: AGENT_ID,
        dreamStore,
        compressionStore: archiveStore,
        sessionLog: sharedLog,
      },
    ).runner

    const text = await collectText(wakeRunner.wake(wakeSession))
    expect(text).toBe("woke")

    const afterWake = await sharedLog.read(wakeSession)
    expect(afterWake.some(e => e.event.kind === "page_in")).toBe(false)
    expect(sawRecallOnWake).toBe(true)
    expect(afterWake.some(e => e.event.kind === "run_terminal")).toBe(true)
  })
})
