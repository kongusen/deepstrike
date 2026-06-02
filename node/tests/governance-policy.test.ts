import { governancePolicyToKernelEvent } from "../src/governance.js"

describe("governancePolicyToKernelEvent", () => {
  it("maps a declarative policy to the snake_case kernel event", () => {
    const event = governancePolicyToKernelEvent({
      defaultAction: "deny",
      rules: [{ pattern: "danger.*", action: "deny" }, { pattern: "fs.*", action: "ask_user" }],
      vetoes: ["rm_rf"],
      rateLimits: [{ tool: "fetch", maxCalls: 3, windowMs: 60_000 }],
      constraints: [
        { kind: "required", tool: "write", path: "path" },
        { kind: "enum", tool: "mode", path: "kind", values: ["a", "b"] },
        { kind: "range", tool: "scale", path: "n", min: 0, max: 10 },
      ],
    })

    expect(event).toEqual({
      kind: "load_governance_policy",
      default_action: "deny",
      rules: [
        { tool_pattern: "danger.*", action: "deny" },
        { tool_pattern: "fs.*", action: "ask_user" },
      ],
      vetoed_tools: ["rm_rf"],
      rate_limits: [{ tool: "fetch", max_calls: 3, window_ms: 60_000 }],
      constraints: [
        { kind: "required", tool: "write", path: "path" },
        { kind: "enum", tool: "mode", path: "kind", values: ["a", "b"] },
        { kind: "range", tool: "scale", path: "n", min: 0, max: 10 },
      ],
    })
  })

  it("omits defaults and empty collections cleanly", () => {
    const event = governancePolicyToKernelEvent({ rules: [{ pattern: "*", action: "allow" }] })
    expect(event).toEqual({
      kind: "load_governance_policy",
      rules: [{ tool_pattern: "*", action: "allow" }],
      vetoed_tools: [],
      rate_limits: [],
      constraints: [],
    })
    expect(event).not.toHaveProperty("default_action")
  })

  it("only includes range bounds that are provided", () => {
    const event = governancePolicyToKernelEvent({
      constraints: [{ kind: "range", tool: "t", path: "p", max: 5 }],
    })
    const constraints = (event as { constraints: Array<Record<string, unknown>> }).constraints
    expect(constraints[0]).toEqual({ kind: "range", tool: "t", path: "p", max: 5 })
    expect(constraints[0]).not.toHaveProperty("min")
  })
})
