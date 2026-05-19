import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { InMemorySessionLog, FileSessionLog } from "../../src/runtime/session-log.js"

describe("InMemorySessionLog", () => {
  it("append returns monotonic seq starting at 0", async () => {
    const log = new InMemorySessionLog()
    const s0 = await log.append("s1", { kind: "run_started", run_id: "r1", goal: "hi", criteria: [] })
    const s1 = await log.append("s1", {
      kind: "llm_completed",
      turn: 0,
      content: "ok",
      tool_calls: [],
    })
    expect(s0).toBe(0)
    expect(s1).toBe(1)
    expect(await log.latestSeq("s1")).toBe(1)
  })

  it("read filters by fromSeq", async () => {
    const log = new InMemorySessionLog()
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("s1", { kind: "llm_completed", turn: 0, content: "b", tool_calls: [] })
    await log.append("s1", { kind: "run_terminal", reason: "completed", turns_used: 1, total_tokens: 10 })

    const tail = await log.read("s1", 1)
    expect(tail).toHaveLength(2)
    expect(tail[0].seq).toBe(1)
    expect(tail[1].event.kind).toBe("run_terminal")
  })

  it("isolates sessions", async () => {
    const log = new InMemorySessionLog()
    await log.append("a", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("b", { kind: "run_started", run_id: "r2", goal: "b", criteria: [] })
    expect((await log.read("a")).length).toBe(1)
    expect((await log.read("b")).length).toBe(1)
  })

  it("latestSeq is -1 for unknown session", async () => {
    const log = new InMemorySessionLog()
    expect(await log.latestSeq("missing")).toBe(-1)
  })
})

describe("FileSessionLog", () => {
  let dir: string

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "ds-session-log-"))
  })

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true })
  })

  it("persists and reloads events", async () => {
    const log = new FileSessionLog(dir)
    await log.append("sess-1", { kind: "run_started", run_id: "r1", goal: "persist", criteria: [] })
    await log.append("sess-1", {
      kind: "tool_completed",
      turn: 1,
      results: [{ call_id: "c1", output: "pong", is_error: false }],
    })

    const log2 = new FileSessionLog(dir)
    const events = await log2.read("sess-1")
    expect(events).toHaveLength(2)
    expect(events[1].event.kind).toBe("tool_completed")
  })

  it("read returns empty for missing session file", async () => {
    const log = new FileSessionLog(dir)
    expect(await log.read("no-such-session")).toEqual([])
  })
})
