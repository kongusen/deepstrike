import { validateToolArguments } from "../src/tools/index.js"

describe("validateToolArguments — additionalProperties", () => {
  it("additionalProperties:true keeps arbitrary nested keys", () => {
    const schema = JSON.stringify({
      type: "object",
      properties: {
        bag: { type: "object", additionalProperties: true, properties: { kind: { type: "string" } } },
      },
    })
    const args = { bag: { kind: "a", anyKey: { nested: 1 }, x: [1, 2] } }
    const r = validateToolArguments(schema, args)
    expect(r.error).toBeUndefined()
    // arbitrary keys survive untouched
    expect(args.bag).toEqual({ kind: "a", anyKey: { nested: 1 }, x: [1, 2] })
  })

  it("additionalProperties undefined still strips (back-compat)", () => {
    const schema = JSON.stringify({ type: "object", properties: { a: { type: "string" } } })
    const args: Record<string, unknown> = { a: "x", extra: 1 }
    const r = validateToolArguments(schema, args)
    expect(r.error).toBeUndefined()
    expect(r.repaired).toBe(true)
    expect(args).toEqual({ a: "x" }) // extra still trimmed
  })

  it("additionalProperties:false strips like the default", () => {
    const schema = JSON.stringify({ type: "object", properties: { a: { type: "string" } }, additionalProperties: false })
    const args: Record<string, unknown> = { a: "x", extra: 1 }
    validateToolArguments(schema, args)
    expect(args).toEqual({ a: "x" })
  })

  it("additionalProperties as a sub-schema validates and repairs extra values", () => {
    const schema = JSON.stringify({
      type: "object",
      properties: {},
      additionalProperties: { type: "number" },
    })
    const args: Record<string, unknown> = { a: "10", b: 2 } // "10" auto-cast to 10
    const r = validateToolArguments(schema, args)
    expect(r.error).toBeUndefined()
    expect(r.repaired).toBe(true)
    expect(args).toEqual({ a: 10, b: 2 })
  })

  it("additionalProperties sub-schema rejects a non-matching extra value", () => {
    const schema = JSON.stringify({
      type: "object",
      properties: {},
      additionalProperties: { type: "number" },
    })
    const args: Record<string, unknown> = { a: { not: "a number" } }
    const r = validateToolArguments(schema, args)
    expect(r.error).toBeDefined()
  })
})

describe("validateToolArguments — oneOf / anyOf", () => {
  const polymorphic = JSON.stringify({
    type: "object",
    properties: {
      text: { oneOf: [{ type: "string" }, { type: "object", properties: { path: { type: "string" } }, required: ["path"] }] },
    },
    required: ["text"],
  })

  it("accepts the scalar branch", () => {
    const args = { text: "hello" }
    const r = validateToolArguments(polymorphic, args)
    expect(r.error).toBeUndefined()
    expect(args.text).toBe("hello")
  })

  it("accepts the object (binding) branch", () => {
    const args = { text: { path: "/k" } }
    const r = validateToolArguments(polymorphic, args)
    expect(r.error).toBeUndefined()
    expect(args.text).toEqual({ path: "/k" })
  })

  it("rejects a value matching no branch", () => {
    const args = { text: 123 }
    const r = validateToolArguments(polymorphic, args)
    expect(r.error).toBeDefined()
  })

  it("does not let a failed branch's repair pollute the next branch", () => {
    // first branch would coerce "5" -> 5 then fail (wrong shape); second branch keeps the string
    const schema = JSON.stringify({
      type: "object",
      properties: {
        v: { anyOf: [{ type: "object", properties: { n: { type: "number" } }, required: ["n"] }, { type: "string" }] },
      },
      required: ["v"],
    })
    const args = { v: "5" }
    const r = validateToolArguments(schema, args)
    expect(r.error).toBeUndefined()
    expect(args.v).toBe("5") // string branch wins, untouched by the object branch's probe
  })
})
