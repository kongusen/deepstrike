/**
 * Plan task 9.4 · transcript search over recorded session events (mirrors the search half of
 * node/tests/consolidate.test.ts; the run half needs the real kernel and is covered in Node).
 */
import { searchTranscripts, transcriptLines, transcriptSearchTool } from "../src/memory/consolidate.js"
import type { SessionEvent, SessionLog } from "../src/runtime/session-log.js"

const events = [
  { kind: "run_started", run_id: "r1", goal: "count the E503 errors", criteria: [] },
  { kind: "llm_completed", turn: 1, content: "reading the log", tool_calls: [{ id: "c1", name: "read", arguments: "{\"path\":\"logs/svc-03.log\"}" }] },
  { kind: "tool_completed", turn: 1, results: [{ call_id: "c1", output: "2026-10-02 ERROR code=E503 req=r120 upstream timeout (retry)", is_error: false }] },
].map((event, seq) => ({ seq, event: event as unknown as SessionEvent }))

const sessionLog = { read: async () => events } as unknown as SessionLog

describe("transcript search (plan task 9.4)", () => {
  it("labels tool results with their call and ranks by query terms", () => {
    const lines = transcriptLines("s1", events)
    expect(lines.map(line => line.source)).toEqual(["goal", "model", "c1 read {\"path\":\"logs/svc-03.log\"}"])
    const found = searchTranscripts(lines, "E503 retry")
    expect(found[0]?.source).toContain("c1 read")
  })

  it("searches only the sessions it was given", async () => {
    const search = transcriptSearchTool({ sessionLog, sessionIds: ["s1"] })
    expect(await search.execute({ query: "E503 retry" })).toContain("[s1 · turn 1 · c1 read")
    expect(await search.execute({ query: "E503", session_id: "s2" })).toBe("no matching lines")
  })
})
