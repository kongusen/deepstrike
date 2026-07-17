import { executeTools, tool, validateToolArguments } from "../src/tools/index.js"

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

describe("validateToolArguments — union at the schema ROOT", () => {
  // inline_result-style discriminated union: object root (providers require it) with the
  // variant constraints in a oneOf sibling.
  const inlineResultShaped = JSON.stringify({
    type: "object",
    properties: {
      kind: { enum: ["edit", "discuss"] },
      delivery: { enum: ["content", "tool"] },
      documentContent: {},
      summary: { type: "string" },
      assistantMessage: { type: "string" },
    },
    required: ["kind"],
    oneOf: [
      {
        type: "object",
        properties: { kind: { const: "edit" }, delivery: { const: "content" }, documentContent: { type: "string" }, summary: { type: "string" } },
        required: ["kind", "delivery", "documentContent"],
      },
      {
        type: "object",
        properties: { kind: { const: "edit" }, delivery: { const: "tool" }, documentContent: { type: "null" }, summary: { type: "string" } },
        required: ["kind", "delivery", "documentContent"],
      },
      {
        type: "object",
        properties: { kind: { const: "discuss" }, assistantMessage: { type: "string" } },
        required: ["kind", "assistantMessage"],
      },
    ],
  })

  it("const discriminates branches — the wrong branch can no longer win and strip the right branch's keys", () => {
    // Without const checking, branch 1 (kind=edit) matched this discuss-shaped call on
    // required+type alone and stripped assistantMessage — silent data mangling.
    const args: Record<string, unknown> = {
      kind: "discuss", delivery: "content", documentContent: "stray", assistantMessage: "the actual answer",
    }
    const r = validateToolArguments(inlineResultShaped, args)
    expect(r.error).toBeUndefined()
    expect(r.args).toEqual({ kind: "discuss", assistantMessage: "the actual answer" })
  })

  it('type: "null" is enforced (delivery=tool requires documentContent null)', () => {
    const ok = validateToolArguments(inlineResultShaped, { kind: "edit", delivery: "tool", documentContent: null })
    expect(ok.error).toBeUndefined()
    const bad = validateToolArguments(inlineResultShaped, { kind: "edit", delivery: "tool", documentContent: "not null" })
    expect(bad.error).toBeDefined()
  })

  it("returns the accepted branch's value as `args` — in-place mutation misses union roots", () => {
    const original: Record<string, unknown> = { kind: "discuss", assistantMessage: "hi", hallucinated: true }
    const r = validateToolArguments(inlineResultShaped, original)
    expect(r.error).toBeUndefined()
    expect(r.repaired).toBe(true)
    // The repair (key strip) lives on the returned clone, NOT the original reference.
    expect(r.args).toEqual({ kind: "discuss", assistantMessage: "hi" })
    expect(original.hallucinated).toBe(true)
  })

  it("executeTools hands the handler the repaired union-root args (was: original, repairs lost)", async () => {
    const seen: Record<string, unknown>[] = []
    const t = tool("finish", "terminal", JSON.parse(inlineResultShaped), args => {
      seen.push(args)
      return "ok"
    })
    const results = await executeTools(
      [{ id: "c1", name: "finish", arguments: JSON.stringify({ kind: "discuss", assistantMessage: "done", hallucinated: 1 }) }],
      new Map([["finish", t]]),
    )
    expect(results[0]!.isError).toBe(false)
    expect(seen[0]).toEqual({ kind: "discuss", assistantMessage: "done" })
  })
})

describe("validateToolArguments — constraint keywords", () => {
  const wrap = (prop: Record<string, unknown>) =>
    JSON.stringify({ type: "object", properties: { v: prop }, required: ["v"] })

  it("minLength/maxLength enforce string bounds", () => {
    const schema = wrap({ type: "string", minLength: 2, maxLength: 4 })
    expect(validateToolArguments(schema, { v: "ok" }).error).toBeUndefined()
    expect(validateToolArguments(schema, { v: "x" }).error).toBe("$.v must be at least 2 characters")
    expect(validateToolArguments(schema, { v: "toolong" }).error).toBe("$.v must be at most 4 characters")
  })

  it("pattern enforces an unanchored regex; an invalid author regex never fails the call", () => {
    const schema = wrap({ type: "string", pattern: "^[a-z]+$" })
    expect(validateToolArguments(schema, { v: "abc" }).error).toBeUndefined()
    expect(validateToolArguments(schema, { v: "ABC" }).error).toBe("$.v must match pattern ^[a-z]+$")
    const badRegex = wrap({ type: "string", pattern: "([" })
    expect(validateToolArguments(badRegex, { v: "anything" }).error).toBeUndefined()
  })

  it("minimum/maximum and exclusive bounds enforce numeric ranges", () => {
    const schema = wrap({ type: "number", minimum: 0, exclusiveMaximum: 10 })
    expect(validateToolArguments(schema, { v: 0 }).error).toBeUndefined()
    expect(validateToolArguments(schema, { v: -1 }).error).toBe("$.v must be >= 0")
    expect(validateToolArguments(schema, { v: 10 }).error).toBe("$.v must be < 10")
  })

  it("bounds apply after the string→number auto-cast", () => {
    const schema = wrap({ type: "integer", minimum: 1 })
    const args: Record<string, unknown> = { v: "0" }
    expect(validateToolArguments(schema, args).error).toBe("$.v must be >= 1")
  })

  it("minItems/maxItems enforce array cardinality", () => {
    const schema = wrap({ type: "array", items: { type: "string" }, minItems: 1, maxItems: 2 })
    expect(validateToolArguments(schema, { v: ["a"] }).error).toBeUndefined()
    expect(validateToolArguments(schema, { v: [] }).error).toBe("$.v must have at least 1 items")
    expect(validateToolArguments(schema, { v: ["a", "b", "c"] }).error).toBe("$.v must have at most 2 items")
  })

  it("not rejects the disallowed shape without leaking probe repairs", () => {
    const schema = wrap({ type: "string", not: { enum: ["forbidden"] } })
    expect(validateToolArguments(schema, { v: "fine" }).error).toBeUndefined()
    expect(validateToolArguments(schema, { v: "forbidden" }).error).toBe("$.v must not match the disallowed shape")
  })

  it("minLength inside a union branch participates in discrimination", () => {
    // discuss-style: an empty assistantMessage no longer matches the branch that requires one.
    const schema = JSON.stringify({
      type: "object",
      properties: { m: { type: "string" } },
      oneOf: [
        { type: "object", properties: { m: { type: "string", minLength: 1 } }, required: ["m"] },
      ],
    })
    expect(validateToolArguments(schema, { m: "hello" }).error).toBeUndefined()
    expect(validateToolArguments(schema, { m: "" }).error).toBe("$ does not match any allowed shape")
  })
})

describe("tool() registration guard", () => {
  it("rejects a parameters root that is not type object (providers 400 on it at call time)", () => {
    expect(() => tool("bad", "d", { oneOf: [{ type: "string" }] }, () => "x"))
      .toThrow(/root type "object"/)
    expect(() => tool("worse", "d", { type: null } as unknown as Record<string, unknown>, () => "x"))
      .toThrow(/root type "object"/)
  })
})

describe("validateToolArguments — coerceItemArray (array auto-cast)", () => {
  const opsSchema = JSON.stringify({
    type: "object",
    properties: {
      ops: { type: "array", items: { type: "object", properties: { op: { type: "string" } }, required: ["op"] } },
    },
    required: ["ops"],
  })

  it("unwraps { item: [...] } into the array", () => {
    const args: any = { ops: { item: [{ op: "add" }, { op: "remove" }] } }
    const r = validateToolArguments(opsSchema, args)
    expect(r.error).toBeUndefined()
    expect(r.repaired).toBe(true)
    expect(args.ops).toEqual([{ op: "add" }, { op: "remove" }])
  })

  it("unwraps { items: [...] } too", () => {
    const args: any = { ops: { items: [{ op: "add" }] } }
    expect(validateToolArguments(opsSchema, args).error).toBeUndefined()
    expect(args.ops).toEqual([{ op: "add" }])
  })

  it("wraps { item: {obj} } into a single-element array (the model's lucky guess)", () => {
    const args: any = { ops: { item: { op: "add" } } }
    expect(validateToolArguments(opsSchema, args).error).toBeUndefined()
    expect(args.ops).toEqual([{ op: "add" }])
  })

  it("wraps a lone object into a single-element array", () => {
    const args: any = { ops: { op: "add" } }
    expect(validateToolArguments(opsSchema, args).error).toBeUndefined()
    expect(args.ops).toEqual([{ op: "add" }])
  })

  it("restores precise per-element errors after coercion (vs blunt 'must be array')", () => {
    const args: any = { ops: { item: { path: "/x" } } } // element missing required `op`
    expect(validateToolArguments(opsSchema, args).error).toBe("$.ops[0].op is required")
  })

  it("leaves a well-formed array untouched", () => {
    const args: any = { ops: [{ op: "add" }] }
    const r = validateToolArguments(opsSchema, args)
    expect(r.error).toBeUndefined()
    expect(r.repaired).toBe(false)
    expect(args.ops).toEqual([{ op: "add" }])
  })
})
