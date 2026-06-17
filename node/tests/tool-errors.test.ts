import { jest } from "@jest/globals"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool, streamingTool, executeTools } from "../src/tools/index.js"
import { safeTool, ok, fail, ToolError, formatToolError } from "../src/tools/errors.js"
import type { ToolAuditFailedEvent, ToolResultEvent } from "../src/types.js"

describe("formatToolError", () => {
  it("returns the message for a plain Error (no Error: prefix)", () => {
    expect(formatToolError(new Error("bad input"))).toBe("bad input")
  })

  it("returns JSON for an Error carrying code / hint / cause", () => {
    const err = new Error("no such section") as Error & { code: string; hint: string }
    err.code = "not_found"
    err.hint = "call document_outline first"
    const out = formatToolError(err)
    expect(JSON.parse(out)).toEqual({
      message: "no such section",
      code: "not_found",
      hint: "call document_outline first",
    })
  })

  it("returns JSON for a plain object (replaces [object Object])", () => {
    const out = formatToolError({ kind: "weird", n: 1 })
    expect(JSON.parse(out)).toEqual({ kind: "weird", n: 1 })
  })

  it("returns the string unchanged for a string throw", () => {
    expect(formatToolError("boom")).toBe("boom")
  })

  it("handles null and undefined without crashing", () => {
    expect(formatToolError(null)).toBe("null")
    expect(formatToolError(undefined)).toBe("undefined")
  })

  it("propagates an Error cause's message", () => {
    const inner = new Error("disk full")
    const outer = new Error("write failed", { cause: inner })
    const out = formatToolError(outer)
    expect(JSON.parse(out)).toMatchObject({ message: "write failed", cause: "disk full" })
  })
})

describe("safeTool", () => {
  it("wraps plain return data in a success envelope", async () => {
    const t = safeTool("echo", "Echo", { type: "object", properties: { x: { type: "string" } }, required: ["x"] },
      ({ x }) => x)
    const [r] = await executeTools([{ id: "1", name: "echo", arguments: JSON.stringify({ x: "hi" }) }], new Map([["echo", t]]))
    expect(r.isError).toBe(false)
    expect(JSON.parse(r.output)).toEqual({ success: true, data: "hi" })
  })

  it("passes through an envelope returned by ok()/fail()", async () => {
    const t = safeTool("lookup", "Lookup", { type: "object", properties: { id: { type: "string" } }, required: ["id"] },
      ({ id }) => id === "good" ? ok({ found: true }) : fail("not_found", `no row ${id}`, "list rows via /index"))
    const [okR] = await executeTools([{ id: "1", name: "lookup", arguments: JSON.stringify({ id: "good" }) }], new Map([["lookup", t]]))
    expect(JSON.parse(okR.output)).toEqual({ success: true, data: { found: true } })
    const [badR] = await executeTools([{ id: "2", name: "lookup", arguments: JSON.stringify({ id: "missing" }) }], new Map([["lookup", t]]))
    // executeTools uses isError = catch path; the envelope itself encodes failure
    expect(JSON.parse(badR.output)).toEqual({
      success: false, code: "not_found", error: "no row missing", hint: "list rows via /index",
    })
  })

  it("turns a thrown ToolError into a fail envelope with code+hint", async () => {
    const t = safeTool("section_read", "Read", { type: "object", properties: { heading: { type: "string" } }, required: ["heading"] },
      ({ heading }) => {
        throw new ToolError(`no section "${heading}"`, { code: "not_found", hint: "call document_outline to list valid headings" })
      })
    const [r] = await executeTools([{ id: "1", name: "section_read", arguments: JSON.stringify({ heading: "X" }) }], new Map([["section_read", t]]))
    expect(r.isError).toBe(false)  // success-shape — envelope encodes failure
    expect(JSON.parse(r.output)).toEqual({
      success: false,
      code: "not_found",
      error: 'no section "X"',
      hint: "call document_outline to list valid headings",
    })
  })

  it("turns a plain Error throw into a fail envelope with code='internal'", async () => {
    const t = safeTool("crash", "Crash", { type: "object", properties: {} },
      () => { throw new Error("kaboom") })
    const [r] = await executeTools([{ id: "1", name: "crash", arguments: "{}" }], new Map([["crash", t]]))
    expect(JSON.parse(r.output)).toEqual({ success: false, code: "internal", error: "kaboom" })
  })

  it("honors err.code / err.hint on a non-ToolError throw", async () => {
    const t = safeTool("conflict", "Conflict", { type: "object", properties: {} },
      () => {
        const e = new Error("write conflict") as Error & { code: string; hint: string }
        e.code = "conflict"
        e.hint = "re-read before write"
        throw e
      })
    const [r] = await executeTools([{ id: "1", name: "conflict", arguments: "{}" }], new Map([["conflict", t]]))
    expect(JSON.parse(r.output)).toEqual({ success: false, code: "conflict", error: "write conflict", hint: "re-read before write" })
  })
})

describe("execution-plane error-aware serialization", () => {
  it("returns Error.message (no 'Error: ' prefix) for a classic tool throw", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("bad", "Bad", { type: "object", properties: {} }, () => { throw new Error("disk full") }))
    const events: ToolResultEvent[] = []
    for await (const e of plane.executeAll([{ id: "1", name: "bad", arguments: "{}" }], {})) {
      if (e.type === "tool_result") events.push(e as ToolResultEvent)
    }
    expect(events).toHaveLength(1)
    expect(events[0].isError).toBe(true)
    expect(events[0].content).toBe("disk full")
  })

  it("emits JSON for a thrown coded Error (no more [object Object])", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("coded", "Coded", { type: "object", properties: {} }, () => {
      const e = new Error("nothing matched") as Error & { code: string }
      e.code = "not_found"
      throw e
    }))
    const events: ToolResultEvent[] = []
    for await (const e of plane.executeAll([{ id: "1", name: "coded", arguments: "{}" }], {})) {
      if (e.type === "tool_result") events.push(e as ToolResultEvent)
    }
    expect(events[0].isError).toBe(true)
    expect(JSON.parse(events[0].content)).toMatchObject({ message: "nothing matched", code: "not_found" })
  })
})

describe("ctx.audit best-effort", () => {
  it("does NOT flip isError when an audit-side-effect throws, and emits tool_audit_failed", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("module_write", "Write", { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
      async ({ path }, ctx) => {
        // main work succeeds
        // best-effort audit fails
        await ctx?.audit?.("record-patch", () => { throw new Error("audit store down") })
        return JSON.stringify({ written: path })
      }))
    const events: Array<ToolResultEvent | ToolAuditFailedEvent> = []
    for await (const e of plane.executeAll([{ id: "1", name: "module_write", arguments: JSON.stringify({ path: "/x" }) }], {})) {
      if (e.type === "tool_result" || e.type === "tool_audit_failed") {
        events.push(e as ToolResultEvent | ToolAuditFailedEvent)
      }
    }
    const audit = events.find(e => e.type === "tool_audit_failed") as ToolAuditFailedEvent | undefined
    const result = events.find(e => e.type === "tool_result") as ToolResultEvent | undefined
    expect(audit).toBeDefined()
    expect(audit!.label).toBe("record-patch")
    expect(audit!.error).toBe("audit store down")
    expect(result).toBeDefined()
    expect(result!.isError).toBe(false)  // the foot-gun is fixed: write is reported as success
    expect(JSON.parse(result!.content)).toEqual({ written: "/x" })
  })

  it("flushes audit failures recorded before a subsequent throw", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("partial", "Partial", { type: "object", properties: {} },
      async (_, ctx) => {
        await ctx?.audit?.("metric", () => { throw new Error("metric collector down") })
        throw new Error("then main work failed")
      }))
    const events: Array<ToolResultEvent | ToolAuditFailedEvent> = []
    for await (const e of plane.executeAll([{ id: "1", name: "partial", arguments: "{}" }], {})) {
      if (e.type === "tool_result" || e.type === "tool_audit_failed") events.push(e as ToolResultEvent | ToolAuditFailedEvent)
    }
    expect(events.some(e => e.type === "tool_audit_failed" && (e as ToolAuditFailedEvent).label === "metric")).toBe(true)
    const r = events.find(e => e.type === "tool_result") as ToolResultEvent
    expect(r.isError).toBe(true)
    expect(r.content).toBe("then main work failed")
  })
})

describe("streaming-tool throw convention", () => {
  it("a streaming tool that throws mid-stream produces a tool_result with error message", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(streamingTool("stream_fail", "Stream fail", { type: "object", properties: {} },
      async function* () {
        yield { type: "text", text: "starting..." }
        throw new Error("midway crash")
      }))
    const events: ToolResultEvent[] = []
    for await (const e of plane.executeAll([{ id: "1", name: "stream_fail", arguments: "{}" }], {})) {
      if (e.type === "tool_result") events.push(e as ToolResultEvent)
    }
    expect(events).toHaveLength(1)
    expect(events[0].isError).toBe(true)
    expect(events[0].content).toBe("midway crash")
  })

  it("warns once when a streaming tool yields a failure-shaped chunk", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(streamingTool("legacy_stream", "Legacy stream", { type: "object", properties: {} },
      async function* () {
        yield { type: "text", text: JSON.stringify({ success: false, code: "not_found", error: "x" }) }
      }))
    const warnSpy = jest.spyOn(console, "warn").mockImplementation(() => {})
    try {
      const events: ToolResultEvent[] = []
      for await (const e of plane.executeAll([{ id: "1", name: "legacy_stream", arguments: "{}" }], {})) {
        if (e.type === "tool_result") events.push(e as ToolResultEvent)
      }
      // The chunk is NOT auto-converted to isError (that's the foot-gun the warning calls out).
      expect(events[0].isError).toBe(false)
      // But the warning fired so the author gets pushed to throw next time.
      expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining('streaming tool "legacy_stream" yielded a failure-shaped chunk'))
    } finally {
      warnSpy.mockRestore()
    }
  })
})
