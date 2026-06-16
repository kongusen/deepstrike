import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, StreamEvent, ToolSchema } from "../src/types.js"

/**
 * P1-B B3 end-to-end: loading a skill that declares `allowed_tools` narrows the toolset the kernel
 * exposes on the NEXT turn to `meta ∪ stable-core ∪ allowed_tools`. The skill's own load turn is
 * still unnarrowed (it only takes effect once active). Meta-tools stay so more skills can load.
 */
function toolsPerTurnProvider(captured: string[][]): LLMProvider {
  let call = 0
  const record = (tools: ToolSchema[]) => captured.push(tools.map(t => t.name))
  return {
    async complete(_ctx, tools: ToolSchema[]): Promise<Message> {
      record(tools)
      return { role: "assistant", content: "done" }
    },
    async *stream(_ctx, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
      record(tools)
      call += 1
      if (call === 1) {
        yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
      } else {
        yield { type: "text_delta", delta: "done" }
      }
    },
  }
}

const baseTools = () => [
  tool("read", "read", { type: "object", properties: {} }, async () => "r"),
  tool("write", "write", { type: "object", properties: {} }, async () => "w"),
  tool("bash", "bash", { type: "object", properties: {} }, async () => "b"),
  tool("grep", "grep", { type: "object", properties: {} }, async () => "g"),
]

describe("P1-B B3: skill-activated tool gating (end-to-end)", () => {
  it("narrows the exposed toolset after a skill with allowed_tools loads", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-gate-skill-"))
    await writeFile(
      join(dir, "debug.md"),
      "---\nname: debug\ndescription: Debug helper\nallowed_tools: read, grep\n---\nDebug guidance.",
    )

    const perTurn: string[][] = []
    const { runner } = createRunner(toolsPerTurnProvider(perTurn), baseTools(), {
      skillDir: dir,
      stableCoreToolIds: ["bash"], // always exposed under gating
    })
    await collectText(runner.run({ sessionId: "gate-skill", goal: "debug it" }))

    expect(perTurn.length).toBeGreaterThanOrEqual(2)
    const loadTurn = perTurn[0]
    const afterTurn = perTurn[perTurn.length - 1]

    // Turn 1 (the load turn): not yet narrowed — every base tool visible, plus the skill meta-tool.
    expect(loadTurn).toEqual(expect.arrayContaining(["read", "write", "bash", "grep", "skill"]))

    // Turn 2 (skill active): narrowed to declared (read, grep) ∪ stable-core (bash) ∪ meta (skill).
    expect(afterTurn).toEqual(expect.arrayContaining(["read", "grep", "bash", "skill"]))
    expect(afterTurn).not.toContain("write")
  })

  it("does not narrow when the skill load fails (errs-open)", async () => {
    // The provider loads "debug", but this dir has no such skill ⇒ the load errors ⇒ no activation
    // ⇒ no narrowing. Failed/missing skills must never gate tools.
    const dir = await mkdtemp(join(tmpdir(), "ds-gate-miss-"))
    await writeFile(join(dir, "other.md"), "---\nname: other\ndescription: x\nallowed_tools: read\n---\nbody")

    const perTurn: string[][] = []
    const { runner } = createRunner(toolsPerTurnProvider(perTurn), baseTools(), {
      skillDir: dir,
      stableCoreToolIds: ["bash"],
    })
    await collectText(runner.run({ sessionId: "gate-miss", goal: "go" }))
    for (const t of perTurn) expect(t).toEqual(expect.arrayContaining(["read", "write", "bash", "grep"]))
  })
})
