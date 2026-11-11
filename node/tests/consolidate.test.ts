// Plan task 9.4 · consolidation is an ordinary run: the model searches earlier transcripts with the
// host's `transcript_search` tool and proposes records with `propose_memory`, which reach the
// host's candidate sink through the existing candidate ABI. No kernel mechanism is involved.
import { RuntimeRunner, collectText } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool } from "../src/tools/index.js"
import { InMemoryMemoryCandidateSink } from "../src/memory/candidate-sink.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"
import {
  consolidateMemory,
  searchTranscripts,
  transcriptLines,
  transcriptSearchTool,
} from "../src/memory/consolidate.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

const scope = { tenant_id: "agent-memory", namespace: "incidents" }

/** One earlier session: the model reads a log whose line it later has to remember. */
async function earlierSession(sessionLog: InMemorySessionLog, sessionId: string) {
  let calls = 0
  const provider: LLMProvider = {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      calls += 1
      if (calls === 1) {
        yield { type: "text_delta", delta: "reading the log" }
        yield { type: "tool_call", id: "c1", name: "read", arguments: { path: "logs/svc-03.log" } }
        return
      }
      yield { type: "text_delta", delta: "req ids ending in 0 are retries and must be skipped" }
    },
  }
  const plane = new LocalExecutionPlane()
  plane.register(tool("read", "read a file", { type: "object", properties: { path: { type: "string" } } },
    () => "2026-10-02 ERROR code=E503 req=r120 upstream timeout (retry)\n2026-10-02 INFO ok"))
  const runner = new RuntimeRunner({ provider, sessionLog, executionPlane: plane, maxTokens: 8_000, baselineToolIds: ["read"] })
  await collectText(runner.run({ sessionId, goal: "count the E503 errors" }))
}

describe("transcript search (plan task 9.4)", () => {
  it("finds tool results, labeled with the call that produced them", async () => {
    const sessionLog = new InMemorySessionLog()
    await earlierSession(sessionLog, "s1")
    const lines = transcriptLines("s1", await sessionLog.read("s1"))
    expect(lines.some(line => line.source === "goal" && line.text.includes("E503"))).toBe(true)
    const found = searchTranscripts(lines, "E503 retry")
    expect(found[0]?.source).toContain("c1 read")
    expect(found[0]?.text).toContain("code=E503 req=r120")

    const search = transcriptSearchTool({ sessionLog, sessionIds: ["s1"] })
    const out = await search.execute({ query: "E503 retry" })
    expect(out).toContain("[s1 · turn")
    expect(await search.execute({ query: "E503", session_id: "other" })).toBe("no matching lines")
  })
})

describe("consolidateMemory (plan task 9.4)", () => {
  it("is an ordinary run whose proposals reach the candidate sink", async () => {
    const sessionLog = new InMemorySessionLog()
    await earlierSession(sessionLog, "s1")
    const offered: string[][] = []
    const seen: string[] = []
    let calls = 0
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        calls += 1
        offered.push(tools.map(t => t.name))
        seen.push(JSON.stringify(context.turns))
        if (calls === 1) {
          yield { type: "tool_call", id: "t1", name: "transcript_search", arguments: { query: "E503 retry" } }
          return
        }
        if (calls === 2) {
          yield {
            type: "tool_call", id: "p1", name: "propose_memory",
            arguments: { name: "e503-retries", kind: "project", content: "E503 lines with req ids ending in 0 are retries; skip them (s1, turn 1)." },
          }
          return
        }
        yield { type: "text_delta", delta: "proposed e503-retries" }
      },
    }
    const sink = new InMemoryMemoryCandidateSink()
    const result = await consolidateMemory({
      provider, sessionLog, sessionIds: ["s1"], maxTokens: 8_000, maxTurns: 6,
      agentId: "agent-memory", memoryScope: scope, memoryStore: new InMemoryMemoryStore(),
      memoryCandidateSink: sink, memoryRuntimePolicy: { retrieval: "host", modelWrites: "candidate_sink" },
    })
    expect(offered[0]).toEqual(expect.arrayContaining(["transcript_search", "propose_memory"]))
    expect(seen[1]).toContain("code=E503 req=r120")
    expect(sink.candidates().map(candidate => candidate.record.content)).toEqual([
      "E503 lines with req ids ending in 0 are retries; skip them (s1, turn 1).",
    ])
    expect(result).toEqual({ sessionId: "consolidate-1", text: "proposed e503-retries" })
  })
})
