import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

/** Flatten everything the model can read out of a rendered context into one searchable string. */
function contextText(ctx: RenderedContext): string {
  return JSON.stringify([ctx.systemText, ctx.systemKnowledge, ctx.turns, ctx.stateTurn])
}

/**
 * P0-A tool gating end-to-end: a static per-run tool profile (`allowedToolIds`) must restrict the
 * tool schemas the kernel hands the provider each turn — proving the SDK-side run_spec synthesis
 * lowers correctly to the kernel `capability_filter`. The kernel-side filter itself is unit-tested
 * in `state_machine::tests::top_level_run_capability_filter_gates_exposed_tools`.
 */
function toolCapturingProvider(captured: { tools: string[] }): LLMProvider {
  return {
    async complete(_ctx: RenderedContext, tools: ToolSchema[]): Promise<Message> {
      captured.tools = tools.map(t => t.name)
      return { role: "assistant", content: "done" }
    },
    async *stream(_ctx: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
      captured.tools = tools.map(t => t.name)
      yield { type: "text_delta", delta: "done" }
    },
  }
}

const baseTools = () => [
  tool("read", "read", { type: "object", properties: {} }, async () => "r"),
  tool("write", "write", { type: "object", properties: {} }, async () => "w"),
  tool("bash", "bash", { type: "object", properties: {} }, async () => "b"),
]

describe("P0-A tool gating (allowedToolIds)", () => {
  it("exposes only the allow-listed tools to the provider", async () => {
    const captured = { tools: [] as string[] }
    const { runner } = createRunner(toolCapturingProvider(captured), baseTools(), {
      allowedToolIds: ["read"],
    })
    await collectText(runner.run({ sessionId: "gate-on", goal: "do it" }))
    expect(captured.tools).toContain("read")
    expect(captured.tools).not.toContain("write")
    expect(captured.tools).not.toContain("bash")
  })

  it("keeps the kernel meta-tools exposed alongside the allow-listed tools", async () => {
    // The documented contract: `allowedToolIds` is a *task*-tool profile — the skill/memory/
    // knowledge/update_plan/read_result meta surfaces stay exposed without being listed, so the
    // model can still load a skill, update the plan, or re-read an evicted result.
    const dir = await mkdtemp(join(tmpdir(), "ds-gate-meta-"))
    await writeFile(join(dir, "debug.md"), "---\nname: debug\ndescription: Debug helper\n---\nDebug guidance.")

    const captured = { tools: [] as string[] }
    const { runner } = createRunner(toolCapturingProvider(captured), baseTools(), {
      allowedToolIds: ["read"],
      enablePlanTool: true,
      skillDir: dir,
    })
    await collectText(runner.run({ sessionId: "gate-meta", goal: "do it" }))
    expect(captured.tools).toContain("read")
    expect(captured.tools).not.toContain("write")
    expect(captured.tools).not.toContain("bash")
    expect(captured.tools).toEqual(expect.arrayContaining(["skill", "update_plan"]))
  })

  it("exposes all tools when no profile is set (no config = old behavior)", async () => {
    const captured = { tools: [] as string[] }
    const { runner } = createRunner(toolCapturingProvider(captured), baseTools(), {})
    await collectText(runner.run({ sessionId: "gate-off", goal: "do it" }))
    expect(captured.tools).toEqual(expect.arrayContaining(["read", "write", "bash"]))
  })
})

/**
 * P0 exposure baseline (`baselineToolIds`) end-to-end: the pre-activation surface under the
 * `allowedToolIds` ceiling. Proves the SDK lowers `exposure_baseline` onto the run spec and that the
 * kernel's unified formula holds through a real skill activation:
 *
 *   exposed = meta ∪ ((baseline ∪ stableCore ∪ ⋃ activeSkills.allowed_tools) ∩ ceiling)
 */
describe("P0 exposure baseline (baselineToolIds)", () => {
  /** Records the toolset per turn; loads `skill(debug)` on turn 1, then finishes. */
  function skillLoadingProvider(perTurn: string[][]): LLMProvider {
    let call = 0
    const record = (tools: ToolSchema[]) => perTurn.push(tools.map(t => t.name))
    return {
      async complete(_ctx, tools: ToolSchema[]): Promise<Message> {
        record(tools)
        return { role: "assistant", content: "done" }
      },
      async *stream(_ctx, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        record(tools)
        call += 1
        if (call === 1) yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
        else yield { type: "text_delta", delta: "done" }
      },
    }
  }

  const wideTools = () => [
    ...baseTools(),
    tool("grep", "grep", { type: "object", properties: {} }, async () => "g"),
  ]

  it("starts at the baseline and widens by exactly the activated skill's declared tools", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-baseline-"))
    await writeFile(
      join(dir, "debug.md"),
      "---\nname: debug\ndescription: Debug helper\nallowed_tools: write\n---\nDebug guidance.",
    )

    const perTurn: string[][] = []
    const { runner } = createRunner(skillLoadingProvider(perTurn), wideTools(), {
      skillDir: dir,
      // Ceiling: what this run may EVER expose. `grep` is inside it but never reachable, because
      // neither the baseline nor the skill names it — the ceiling is a bound, not a grant.
      allowedToolIds: ["read", "write", "grep"],
      // Baseline: pre-activation surface. `bash` sits OUTSIDE the ceiling ⇒ D3 silent intersection.
      baselineToolIds: ["read", "bash"],
    })
    await collectText(runner.run({ sessionId: "baseline-widen", goal: "debug it" }))

    expect(perTurn.length).toBeGreaterThanOrEqual(2)
    const before = perTurn[0]
    const after = perTurn[perTurn.length - 1]

    // Turn 1 — narrow: baseline ∩ ceiling = {read}. `write` is reachable-but-not-yet-advertised,
    // which is exactly the expressiveness `allowedToolIds` alone could not deliver.
    expect(before).toContain("read")
    expect(before).not.toContain("write")
    expect(before).not.toContain("grep")
    // D3: a ceiling-external baseline entry silently intersects away — no error, just absent.
    expect(before).not.toContain("bash")
    // Meta stays exempt on the id axis, so the model can still load the skill that widens it.
    expect(before).toContain("skill")

    // Turn 2 — widened by exactly the declaration, still under the ceiling.
    expect(after).toEqual(expect.arrayContaining(["read", "write"]))
    expect(after).not.toContain("grep")
    expect(after).not.toContain("bash")
  })

  it("baselineToolIds: [] is the minimal surface (meta + stable-core only), distinct from unset", async () => {
    const captured = { tools: [] as string[] }
    const { runner } = createRunner(toolCapturingProvider(captured), baseTools(), {
      baselineToolIds: [],
      stableCoreToolIds: ["read"],
      enablePlanTool: true,
    })
    await collectText(runner.run({ sessionId: "baseline-minimal", goal: "do it" }))
    // `[]` is NOT the `allowedToolIds` "empty = no gating" trap: it really means minimal.
    expect(captured.tools).not.toContain("write")
    expect(captured.tools).not.toContain("bash")
    // stable-core survives the minimal baseline (it is a union term of the formula)...
    expect(captured.tools).toContain("read")
    // ...and so do the kernel meta surfaces.
    expect(captured.tools).toContain("update_plan")
  })

  it("unset baseline keeps the legacy surface (no config = old behavior)", async () => {
    const captured = { tools: [] as string[] }
    const { runner } = createRunner(toolCapturingProvider(captured), baseTools(), {
      stableCoreToolIds: ["read"],
      enablePlanTool: true,
    })
    await collectText(runner.run({ sessionId: "baseline-unset", goal: "do it" }))
    expect(captured.tools).toEqual(expect.arrayContaining(["read", "write", "bash"]))
  })
})

/**
 * P1 fail-closed dispatch (`toolDispatchGate`): exposure filtering is now ENFORCED, not just
 * advertised. A call to a tool that is registered on the execution plane but was gated out of this
 * turn's schema never reaches the host — the kernel commits a model-visible `governance_denied`
 * result instead. `"registered"` is the documented escape hatch back to permissive dispatch.
 */
describe("P1 fail-closed dispatch (toolDispatchGate)", () => {
  /** Calls the gated-out `write` on turn 1 (plus an exposed sibling), then finishes. */
  function unexposedCallProvider(contexts: string[]): LLMProvider {
    let call = 0
    return {
      async complete(ctx: RenderedContext): Promise<Message> {
        contexts.push(contextText(ctx))
        return { role: "assistant", content: "done" }
      },
      async *stream(ctx: RenderedContext): AsyncIterable<StreamEvent> {
        contexts.push(contextText(ctx))
        call += 1
        if (call === 1) {
          yield { type: "tool_call", id: "c-allowed", name: "read", arguments: {} }
          yield { type: "tool_call", id: "c-denied", name: "write", arguments: {} }
        } else {
          yield { type: "text_delta", delta: "done" }
        }
      },
    }
  }

  function gatedRunner(gate?: "exposed" | "registered") {
    const ran = { read: false, write: false }
    const contexts: string[] = []
    const tools = [
      tool("read", "read", { type: "object", properties: {} }, async () => { ran.read = true; return "r" }),
      tool("write", "write", { type: "object", properties: {} }, async () => { ran.write = true; return "w" }),
    ]
    const { runner } = createRunner(unexposedCallProvider(contexts), tools, {
      // `write` stays REGISTERED on the plane but outside the exposure ceiling.
      allowedToolIds: ["read"],
      ...(gate === undefined ? {} : { toolDispatchGate: gate }),
    })
    return { runner, ran, contexts }
  }

  it("default gate denies a call to a registered-but-unexposed tool, and never executes it", async () => {
    const { runner, ran, contexts } = gatedRunner()
    await collectText(runner.run({ sessionId: "dispatch-closed", goal: "do it" }))

    expect(ran.write).toBe(false)
    // Allowed siblings in the SAME batch still execute — the gate partitions, it does not abort.
    expect(ran.read).toBe(true)
    // The denial is model-visible and says what to do next (下一请求信息最大化), so the tool_call is
    // answered rather than orphaned.
    const afterDenial = contexts[contexts.length - 1]
    expect(afterDenial).toContain("is not part of this run's toolset")
    expect(afterDenial).toContain("write")
  })

  it('toolDispatchGate: "registered" restores permissive dispatch (the escape hatch)', async () => {
    const { runner, ran, contexts } = gatedRunner("registered")
    await collectText(runner.run({ sessionId: "dispatch-open", goal: "do it" }))

    expect(ran.write).toBe(true)
    expect(ran.read).toBe(true)
    expect(contexts[contexts.length - 1]).not.toContain("is not part of this run's toolset")
  })
})
