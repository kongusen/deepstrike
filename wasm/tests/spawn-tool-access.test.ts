/**
 * `AgentRunSpec.toolAccess` on the wasm spawn path (parity port of the Node/Python change).
 *
 * WASM has no native kernel in tests (per the suite's fake-kernel convention), so rather than drive a
 * child loop we exercise the two load-bearing behaviours through pure seams:
 *
 *  - `agentRunSpecToKernel` must NOT lower `toolAccess` onto the wire (host-side only, like `modelHint`).
 *  - `resolveToolGrants` (the grant-resolution seam `SubAgentOrchestrator.run` calls) picks the
 *    parent plane for `"inherit"`, and for a zero-tool `"filtered"` spawn emits a host-visible warning
 *    — except a workflow node (quarantined deny-all is intentional).
 */
import { agentRunSpecToKernel } from "../src/runtime/types/agent.js"
import { resolveToolGrants } from "../src/runtime/sub-agent-orchestrator.js"
import type { SubAgentRunContext } from "../src/runtime/sub-agent-orchestrator.js"
import type { RuntimeOptions } from "../src/runtime/runner.js"
import type { ExecutionPlane } from "../src/runtime/execution-plane.js"
import type { AgentRunSpec } from "../src/index.js"

const stubPlane = { register() { return this }, unregister() { return this }, schemas: () => [], async *executeAll() {} } as unknown as ExecutionPlane
const parentOpts = { executionPlane: stubPlane } as unknown as RuntimeOptions

const mkCtx = (over: Partial<SubAgentRunContext> = {}): SubAgentRunContext => ({
  parentOpts,
  parentSessionId: "parent",
  spec: {
    identity: { agentId: "worker", sessionId: "worker-s", isSubAgent: true },
    role: "implement",
    isolation: "shared",
    goal: "do the work",
  },
  manifest: {
    kind: "agent_process_changed",
    agent_id: "worker",
    parent_session_id: "parent",
    role: "implement",
    isolation: "shared",
    context_inheritance: "none",
    permitted_capability_ids: [],
  },
  sessionLog: {} as never,
  ...over,
})

/** Run `fn` while capturing anything written to `console.warn`. */
function captureWarnings(fn: () => void): string[] {
  const warnings: string[] = []
  const original = console.warn
  console.warn = (...args: unknown[]) => { warnings.push(args.map(String).join(" ")) }
  try { fn() } finally { console.warn = original }
  return warnings
}

describe("spawn tool access (wasm AgentRunSpec.toolAccess)", () => {
  it("agentRunSpecToKernel omits toolAccess from the wire (host-side only)", () => {
    for (const access of ["inherit", "filtered"] as const) {
      const spec: AgentRunSpec = {
        identity: { agentId: "w", sessionId: "s", isSubAgent: true },
        role: "implement",
        goal: "g",
        toolAccess: access,
      }
      const lowered = agentRunSpecToKernel(spec)
      expect(lowered).not.toHaveProperty("tool_access")
      expect(lowered).not.toHaveProperty("toolAccess")
    }
  })

  it("(a) toolAccess:'inherit' resolves to the parent's plane and never warns", () => {
    const warnings = captureWarnings(() => {
      const { plane } = resolveToolGrants(mkCtx({ toolAccess: "inherit" }))
      expect(plane).toBe(stubPlane)
    })
    expect(warnings.join("\n")).not.toContain("zero tools")
  })

  it("(b) default 'filtered' with no capability resolves to zero tools and warns the host", () => {
    const warnings = captureWarnings(() => {
      resolveToolGrants(mkCtx({ toolAccess: "filtered" }))
    })
    const joined = warnings.join("\n")
    expect(joined).toContain("zero tools")
    expect(joined).toContain("worker")
  })

  it("(c) a workflow node that resolves to zero filtered tools is EXEMPT (intentional quarantine deny-all)", () => {
    const warnings = captureWarnings(() => {
      resolveToolGrants(mkCtx({ toolAccess: "filtered", isWorkflowNode: true }))
    })
    expect(warnings.join("\n")).not.toContain("zero tools")
  })
})
