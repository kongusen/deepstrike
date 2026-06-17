import { jest } from "@jest/globals"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { tool, executeTools } from "../src/tools/index.js"
import { safeTool, ok, fail, ToolError, formatToolError } from "../src/tools/errors.js"
import type { ToolAuditFailedEvent, ToolResultEvent } from "../src/types.js"

describe("formatToolError", () => {
  it("returns the message for a plain Error (no Error: prefix)", () => {
    expect(formatToolError(new Error("bad input"))).toBe("bad input")
  })

  it("returns JSON for an Error carrying code / hint", () => {
    const err = new Error("no such section") as Error & { code: string; hint: string }
    err.code = "not_found"
    err.hint = "call document_outline first"
    expect(JSON.parse(formatToolError(err))).toEqual({
      message: "no such section",
      code: "not_found",
      hint: "call document_outline first",
    })
  })

  it("replaces [object Object] for a plain object throw", () => {
    expect(JSON.parse(formatToolError({ kind: "weird", n: 1 }))).toEqual({ kind: "weird", n: 1 })
  })

  it("passes through strings; handles null/undefined", () => {
    expect(formatToolError("boom")).toBe("boom")
    expect(formatToolError(null)).toBe("null")
    expect(formatToolError(undefined)).toBe("undefined")
  })
})

describe("safeTool", () => {
  it("wraps plain return data in ok envelope", async () => {
    const t = safeTool("echo", "Echo", { type: "object", properties: { x: { type: "string" } }, required: ["x"] },
      ({ x }) => x)
    const [r] = await executeTools([{ id: "1", name: "echo", arguments: JSON.stringify({ x: "hi" }) }], new Map([["echo", t]]))
    expect(r.isError).toBe(false)
    expect(JSON.parse(r.output)).toEqual({ success: true, data: "hi" })
  })

  it("passes through ok()/fail() envelopes", async () => {
    const t = safeTool("lookup", "Lookup", { type: "object", properties: { id: { type: "string" } }, required: ["id"] },
      ({ id }) => id === "good" ? ok({ found: true }) : fail("not_found", `no row ${id}`, "list rows via /index"))
    const [okR] = await executeTools([{ id: "1", name: "lookup", arguments: JSON.stringify({ id: "good" }) }], new Map([["lookup", t]]))
    expect(JSON.parse(okR.output)).toEqual({ success: true, data: { found: true } })
    const [badR] = await executeTools([{ id: "2", name: "lookup", arguments: JSON.stringify({ id: "missing" }) }], new Map([["lookup", t]]))
    expect(JSON.parse(badR.output)).toEqual({
      success: false, code: "not_found", error: "no row missing", hint: "list rows via /index",
    })
  })

  it("turns ToolError throw into fail envelope with code+hint", async () => {
    const t = safeTool("section_read", "Read", { type: "object", properties: { heading: { type: "string" } }, required: ["heading"] },
      ({ heading }) => {
        throw new ToolError(`no section "${heading}"`, { code: "not_found", hint: "call document_outline first" })
      })
    const [r] = await executeTools([{ id: "1", name: "section_read", arguments: JSON.stringify({ heading: "X" }) }], new Map([["section_read", t]]))
    expect(JSON.parse(r.output)).toEqual({
      success: false, code: "not_found", error: 'no section "X"', hint: "call document_outline first",
    })
  })

  it("turns plain Error into fail envelope with code=internal", async () => {
    const t = safeTool("crash", "Crash", { type: "object", properties: {} }, () => { throw new Error("kaboom") })
    const [r] = await executeTools([{ id: "1", name: "crash", arguments: "{}" }], new Map([["crash", t]]))
    expect(JSON.parse(r.output)).toEqual({ success: false, code: "internal", error: "kaboom" })
  })
})

describe("execution-plane error-aware serialization", () => {
  it("returns Error.message (no Error: prefix) for a thrown Error", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("bad", "Bad", { type: "object", properties: {} }, () => { throw new Error("disk full") }))
    const events: ToolResultEvent[] = []
    for await (const e of plane.executeAll([{ id: "1", name: "bad", arguments: "{}" }], {})) {
      if (e.type === "tool_result") events.push(e as ToolResultEvent)
    }
    expect(events[0].isError).toBe(true)
    expect(events[0].content).toBe("disk full")
  })

  it("emits JSON for coded Error (no more [object Object])", async () => {
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
  it("does NOT flip isError when audit-side-effect throws; emits tool_audit_failed", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("module_write", "Write", { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
      async ({ path }, ctx) => {
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
    expect(result!.isError).toBe(false)
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
