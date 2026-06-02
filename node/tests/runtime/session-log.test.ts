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

  it("read filters by primitiveFilter", async () => {
    const log = new InMemorySessionLog()
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("s1", { kind: "page_out", turn: 0, category: "mm", primitive: "mm", summary: "po" })
    await log.append("s1", { kind: "suspended", turn: 1, category: "sched", primitive: "sched", reason: "sus" })
    await log.append("s1", { kind: "tool_gated", turn: 2, category: "syscall", primitive: "syscall", call_id: "c1", tool: "t1", reason: "gated" })

    const mmEvents = await log.read("s1", 0, "mm")
    expect(mmEvents).toHaveLength(1)
    expect(mmEvents[0].event.kind).toBe("page_out")

    const schedEvents = await log.read("s1", 1, "sched")
    expect(schedEvents).toHaveLength(1)
    expect(schedEvents[0].event.kind).toBe("suspended")

    const syscallEvents = await log.read("s1", 0, "syscall")
    expect(syscallEvents).toHaveLength(1)
    expect(syscallEvents[0].event.kind).toBe("tool_gated")
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
