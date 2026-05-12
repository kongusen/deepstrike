/**
 * 03_tools.test.ts — tool(), executeTools(), readFile, LLM tool calling
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { tool, executeTools, readFile, normalizeToolCall } from "@deepstrike/sdk"
import type { ToolResultEvent } from "@deepstrike/sdk"
import { makeAgent, collectEvents, text } from "./helpers.js"

// ─── Offline mechanics ────────────────────────────────────────────────────

describe("tool() factory", () => {
  it("creates correct schema", () => {
    const t = tool("my_tool", "A test tool", { type: "object", properties: {} }, async () => "ok")
    assert.equal(t.schema.name, "my_tool")
    assert.equal(t.schema.description, "A test tool")
    assert.equal(JSON.parse(t.schema.parameters).type, "object")
  })

  it("execute() returns the handler's string", async () => {
    const t = tool("add", "Add", {
      type: "object",
      properties: { a: { type: "number" }, b: { type: "number" } },
      required: ["a", "b"],
    }, ({ a, b }) => String(Number(a) + Number(b)))
    assert.equal(await t.execute({ a: 3, b: 4 }), "7")
  })

  it("execute() propagates exceptions", async () => {
    const t = tool("boom", "Explodes", {}, async () => { throw new Error("kaboom") })
    await assert.rejects(() => t.execute({}), /kaboom/)
  })
})

describe("executeTools()", () => {
  it("runs a known tool", async () => {
    const t = tool("echo", "Echo", { type: "object", properties: { msg: { type: "string" } }, required: ["msg"] },
      ({ msg }) => String(msg))
    const [r] = await executeTools([{ id: "c1", name: "echo", arguments: '{"msg":"hello"}' }], new Map([["echo", t]]))
    assert.equal(r.output, "hello")
    assert.equal(r.isError, false)
  })

  it("returns error for unknown tool", async () => {
    const [r] = await executeTools([{ id: "c2", name: "ghost", arguments: "{}" }], new Map())
    assert.equal(r.isError, true)
    assert.ok(r.output.includes("ghost"))
  })

  it("returns error when tool throws", async () => {
    const bad = tool("fail", "Fails", {}, async () => { throw new Error("oops") })
    const [r] = await executeTools([{ id: "c3", name: "fail", arguments: "{}" }], new Map([["fail", bad]]))
    assert.equal(r.isError, true)
  })

  it("runs multiple tools in parallel", async () => {
    const t1 = tool("a", "A", {}, async () => "aaa")
    const t2 = tool("b", "B", {}, async () => "bbb")
    const registry = new Map([["a", t1], ["b", t2]])
    const results = await executeTools(
      [{ id: "1", name: "a", arguments: "{}" }, { id: "2", name: "b", arguments: "{}" }],
      registry,
    )
    assert.deepEqual(results.map(r => r.output).sort(), ["aaa", "bbb"])
  })
})

describe("readFile built-in tool", () => {
  it("schema has required path field", () => {
    assert.equal(readFile.schema.name, "read_file")
    assert.ok(JSON.parse(readFile.schema.parameters).required.includes("path"))
  })

  it("reads an existing file", async () => {
    const content = await readFile.execute({ path: "../../.env" })
    assert.ok(content.includes("OPENAI"))
  })
})

// ─── LLM tool calling (real API) ─────────────────────────────────────────

describe("Agent with tools", () => {
  it("LLM calls an arithmetic tool and final text includes the result", { timeout: 60_000 }, async () => {
    const calc = tool("calculate", "Perform arithmetic", {
      type: "object",
      properties: {
        op: { type: "string", enum: ["add", "sub", "mul", "div"] },
        a:  { type: "number" },
        b:  { type: "number" },
      },
      required: ["op", "a", "b"],
    }, ({ op, a, b }) => {
      const x = Number(a), y = Number(b)
      const result = op === "add" ? x + y : op === "sub" ? x - y : op === "mul" ? x * y : x / y
      return String(result)
    })

    const agent = makeAgent().register(calc)
    const events = await collectEvents(
      agent.runStreaming("Use the calculate tool to compute 17 * 6. Return only the numeric result."),
    )

    const calcResult = (events.filter(e => e.type === "tool_result") as ToolResultEvent[])
      .find(r => r.name === "calculate")
    assert.ok(calcResult, "calculate must have been called")
    assert.ok(calcResult.content.includes("102"), `tool returned: ${calcResult.content}`)
    assert.ok(text(events).includes("102"), `final text: ${text(events)}`)
  })

  it("tool_call event precedes its tool_result event", { timeout: 60_000 }, async () => {
    const t = tool("ping", "Returns pong", {}, async () => "pong")
    const events = await collectEvents(
      makeAgent().register(t).runStreaming("Call the ping tool and report what it returns."),
    )
    const callIdx   = events.findIndex(e => e.type === "tool_call")
    const resultIdx = events.findIndex(e => e.type === "tool_result")
    if (callIdx !== -1 && resultIdx !== -1) {
      assert.ok(callIdx < resultIdx, "tool_call must precede tool_result")
    }
  })
})
