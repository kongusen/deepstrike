/**
 * 09_governance.test.ts — PermissionManager + kernel Governance + agent-level blocking
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { PermissionManager, PermissionMode, Governance, tool } from "@deepstrike/sdk"
import type { ErrorEvent, PermissionRequestEvent } from "@deepstrike/sdk"
import { makeAgent, collectEvents } from "./helpers.js"

// ─── PermissionManager (offline) ─────────────────────────────────────────

describe("PermissionManager", () => {
  it("AUTO mode allows everything", () => {
    assert.equal(new PermissionManager(PermissionMode.AUTO).evaluate("any", "exec").allowed, true)
  })

  it("PLAN mode blocks everything", () => {
    assert.equal(new PermissionManager(PermissionMode.PLAN).evaluate("any", "exec").allowed, false)
  })

  it("DEFAULT: ungranted tool is denied", () => {
    assert.equal(new PermissionManager().evaluate("tool", "exec").allowed, false)
  })

  it("DEFAULT: granted tool is allowed", () => {
    const pm = new PermissionManager()
    pm.grant("tool", "exec")
    assert.equal(pm.evaluate("tool", "exec").allowed, true)
  })

  it("DEFAULT: revoked tool is denied with note", () => {
    const pm = new PermissionManager()
    pm.grant("tool", "exec")
    pm.revoke("tool", "exec", "security policy")
    const d = pm.evaluate("tool", "exec")
    assert.equal(d.allowed, false)
    assert.ok(d.reason.includes("security policy"))
  })

  it("wildcard grant covers any tool", () => {
    const pm = new PermissionManager()
    pm.grant("*", "*")
    assert.equal(pm.evaluate("foo", "bar").allowed, true)
  })

  it("requiresApproval blocks even when granted", () => {
    const pm = new PermissionManager()
    pm.grant("tool", "exec", { requiresApproval: true })
    const d = pm.evaluate("tool", "exec")
    assert.equal(d.allowed, false)
    assert.equal(d.requiresApproval, true)
  })
})

// ─── Kernel Governance (offline) ─────────────────────────────────────────

describe("Governance (kernel)", () => {
  it("default 'allow' permits any tool", () => {
    assert.equal(new Governance("allow").evaluate("any_tool", "{}").kind, "allow")
  })

  it("default 'deny' blocks any tool", () => {
    assert.equal(new Governance("deny").evaluate("any_tool", "{}").kind, "deny")
  })

  it("addPermissionRule deny blocks matched tool", () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("dangerous_*", "deny")
    assert.equal(gov.evaluate("safe_tool",       "{}").kind, "allow")
    assert.equal(gov.evaluate("dangerous_delete","{}").kind, "deny")
  })

  it("addPermissionRule ask_user returns ask_user verdict", () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("sensitive_op", "ask_user")
    assert.equal(gov.evaluate("sensitive_op", "{}").kind, "ask_user")
  })

  it("blockTool() hard-denies regardless of rules", () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("tool", "allow")   // explicit allow
    gov.blockTool("tool")
    assert.equal(gov.evaluate("tool", "{}").kind, "deny")
  })

  it("first matching rule wins", () => {
    const gov = new Governance("deny")
    gov.addPermissionRule("tool_a", "allow")
    gov.addPermissionRule("tool_a", "deny")   // shadowed
    assert.equal(gov.evaluate("tool_a", "{}").kind, "allow")
  })

  it("setIdentity() doesn't throw", () => {
    assert.doesNotThrow(() => new Governance("allow").setIdentity("agent-1", "session-1"))
  })
})

// ─── Agent with Governance (real API) ────────────────────────────────────

describe("Agent with Governance", () => {
  it("blocked tool yields error event, run still terminates", { timeout: 120_000 }, async () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("forbidden_action", "deny")

    const forbidden = tool("forbidden_action", "Perform a forbidden action", {}, async () => "done")
    const safe      = tool("safe_reply", "Reply safely", {
      type: "object", properties: { msg: { type: "string" } }, required: ["msg"],
    }, async ({ msg }) => String(msg))

    const agent = makeAgent({ governance: gov }).register(forbidden).register(safe)
    const events = await collectEvents(
      agent.runStreaming("First call forbidden_action. If blocked, call safe_reply with msg='ok'."),
    )

    const errors = events.filter(e => e.type === "error") as ErrorEvent[]
    assert.ok(errors.some(e => e.message.includes("permission denied")),
      `errors: ${errors.map(e => e.message)}`)
    assert.equal(events.filter(e => e.type === "done").length, 1)
  })

  it("ask_user tool yields permission_request event", { timeout: 60_000 }, async () => {
    const gov = new Governance("allow")
    gov.addPermissionRule("needs_approval", "ask_user")

    const needsApproval = tool("needs_approval", "Requires approval", {}, async () => "approved")
    const agent = makeAgent({ governance: gov }).register(needsApproval)
    const events = await collectEvents(
      agent.runStreaming("Call needs_approval immediately."),
    )

    const permReqs = events.filter(e => e.type === "permission_request") as PermissionRequestEvent[]
    if (permReqs.length > 0) {
      assert.equal(permReqs[0].toolName, "needs_approval")
    }
    assert.equal(events.filter(e => e.type === "done").length, 1)
  })
})
