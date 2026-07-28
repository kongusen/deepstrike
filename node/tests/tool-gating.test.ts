import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

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
