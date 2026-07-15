import { createRunner, tool } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import type { ArchiveStore } from "../../src/runtime/archive.js"
import type { DreamStore, MemoryRecall } from "../../src/memory/protocols.js"
import type { InMemorySessionLog } from "../../src/runtime/session-log.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../../src/types.js"

const AGENT_ID = "agent-mm-paging"
const MEMORY_SCOPE = { tenant_id: AGENT_ID, namespace: "integration" }
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
    upsert: async () => {},
    saveSession: async () => {},
    search: async (_agentId, query) => {
      if (query.query.toLowerCase().includes("archived")) {
        return [{
          record: {
            record_id: "memory-recall-marker",
            scope: query.scope,
            name: "archived-session-fact",
            kind: "reference",
            content: RECALL_MARKER,
            description: "fixture recalled from an archived session",
            provenance: {
              author: "host",
              trust: "host_verified",
              evidence_refs: [],
            },
            created_at: 1,
            updated_at: 1,
            recall_count: 0,
            confidence: 0.97,
            links: [],
            pinned: false,
          },
          score: 0.97,
          why: "query matched archived-session fixture",
        } satisfies MemoryRecall]
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
        if (providerCalls <= 14) {
          // The provider's measured input is the pressure fact. Drive the planner directly to
          // context collapse so this integration test exercises semantic page-out, not a
          // payload-shape-dependent snip/micro-compaction tier.
          yield { type: "usage", totalTokens: 941, inputTokens: 940, outputTokens: 1 }
          yield { type: "tool_call", id: `bulk${providerCalls}`, name: "bulk", arguments: {} }
          return
        }
        if (providerCalls === 15) {
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
        tool("bulk", "bulk", { type: "object", properties: {} }, () => "z ".repeat(140)),
      ],
      {
        maxTokens: 1024,
        maxTurns: 30,
        agentId: AGENT_ID,
        memoryScope: MEMORY_SCOPE,
        dreamStore: pagingDreamStore(),
        dreamSummarizer: { async summarize() { return "archived session summary" } },
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
        if (compressCalls <= 14) {
          yield { type: "usage", totalTokens: 941, inputTokens: 940, outputTokens: 1 }
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
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "y ".repeat(140))],
      {
        maxTokens: 1024,
        maxTurns: 30,
        agentId: AGENT_ID,
        memoryScope: MEMORY_SCOPE,
        dreamStore,
        dreamSummarizer: { async summarize() { return "archived session summary" } },
        compressionStore: archiveStore,
      },
    ).sessionLog

    const compressRunner = createRunner(
      compressProvider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "y ".repeat(140))],
      {
        maxTokens: 1024,
        maxTurns: 30,
        agentId: AGENT_ID,
        memoryScope: MEMORY_SCOPE,
        dreamStore,
        dreamSummarizer: { async summarize() { return "archived session summary" } },
        compressionStore: archiveStore,
        sessionLog: sharedLog,
        // The script deliberately repeats an identical `bulk()` call 14 turns in a row to force
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
        memoryScope: MEMORY_SCOPE,
        dreamStore,
        dreamSummarizer: { async summarize() { return "archived session summary" } },
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
