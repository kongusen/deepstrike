import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"

import { FileSessionLog, InMemorySessionLog } from "../../src/runtime/session-log.js"

describe("InMemorySessionLog", () => {
  it("keeps a dense business-event sequence independent of KernelJournal", async () => {
    const log = new InMemorySessionLog()
    await log.kernelJournal.compareAndAppend("op-1", undefined, {
      step_seq: 0,
      record_digest: "d0",
      record_bytes: new Uint8Array([1]),
    })

    const first = await log.append("s1", {
      kind: "run_started",
      run_id: "r1",
      goal: "hi",
      criteria: [],
    })
    const second = await log.append("s1", {
      kind: "llm_completed",
      turn: 0,
      content: "ok",
      tool_calls: [],
    })

    expect([first, second]).toEqual([0, 1])
    expect(await log.latestSeq("s1")).toBe(1)
    expect((await log.read("s1")).map(entry => entry.seq)).toEqual([0, 1])
    expect(log).not.toHaveProperty("appendKernelGenesis")
  })

  it("filters by cursor and primitive without crossing sessions", async () => {
    const log = new InMemorySessionLog()
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "a", criteria: [] })
    await log.append("s1", { kind: "tool_started", call_id: "c1", tool: "read" })
    await log.append("s2", { kind: "run_started", run_id: "r2", goal: "b", criteria: [] })

    expect((await log.read("s1", 1)).map(entry => entry.seq)).toEqual([1])
    expect((await log.read("s1", 0, "sched")).map(entry => entry.event.kind)).toEqual([
      "run_started",
      "tool_started",
    ])
    expect((await log.read("s2")).map(entry => entry.event.kind)).toEqual(["run_started"])
    expect(await log.latestSeq("missing")).toBe(-1)
  })
})

describe("FileSessionLog", () => {
  let dir: string

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "deepstrike-session-log-"))
  })

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true })
  })

  it("persists business events and remounts its separate journal", async () => {
    const log = new FileSessionLog(dir)
    await log.append("s1", { kind: "run_started", run_id: "r1", goal: "hi", criteria: [] })
    await log.kernelJournal.compareAndAppend("op-1", undefined, {
      step_seq: 0,
      record_digest: "d0",
      record_bytes: new Uint8Array([1]),
    })

    const reopened = new FileSessionLog(dir)
    expect((await reopened.read("s1")).map(entry => entry.event.kind)).toEqual(["run_started"])
    expect(await reopened.latestSeq("s1")).toBe(0)
    expect(await reopened.kernelJournal.head("op-1")).toEqual({ step_seq: 0, record_digest: "d0" })
  })

  it("serializes concurrent appends within one instance", async () => {
    const log = new FileSessionLog(dir)
    const seqs = await Promise.all(Array.from({ length: 20 }, (_, index) =>
      log.append("s1", {
        kind: "llm_completed",
        turn: index,
        content: String(index),
        tool_calls: [],
      })))

    expect(seqs).toEqual(Array.from({ length: 20 }, (_, index) => index))
    expect((await log.read("s1")).map(entry => entry.seq)).toEqual(seqs)
  })

  it("returns an empty projection for a missing session", async () => {
    const log = new FileSessionLog(dir)
    expect(await log.read("missing")).toEqual([])
    expect(await log.latestSeq("missing")).toBe(-1)
  })
})
