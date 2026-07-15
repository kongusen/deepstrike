/**
 * K3 — skill deactivation + lease. `active_skills` used to be additive-only: a multi-phase run
 * kept every early phase's skill content pinned in knowledge and its allowed_tools unioned into
 * the filter forever. `deactivateSkill()` (host-driven — no model-facing unload) re-widens the
 * toolset at the next provider call and drops the `skill:<name>` knowledge pin at the next
 * compaction boundary; `skillLeaseTurns` does the same automatically after N turns.
 */
import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const SKILL_BODY = "Debug guidance: always reproduce before fixing."

async function skillDir(): Promise<string> {
  const dir = await mkdtemp(join(tmpdir(), "ds-skill-deactivate-"))
  await writeFile(join(dir, "debug.md"), `---\nname: debug\ndescription: Debug helper\n---\n${SKILL_BODY}`)
  return dir
}

describe("skill deactivation (K3)", () => {
  it("deactivateSkill unpins the content at the next boundary; re-activation re-pins", async () => {
    let call = 0
    let afterDeactivation = ""
    let afterReactivation = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
          return
        }
        if (call === 2) {
          // Skill content is pinned now; phase over — the host deactivates it.
          expect(context.systemKnowledge ?? "").toContain(SKILL_BODY)
          await runner.deactivateSkill("debug")
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call <= 10) {
          // Filler turns to cross a compaction boundary (the pin drops there, not immediately).
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call === 11) {
          afterDeactivation = context.systemKnowledge ?? ""
          // Re-activation: a fresh `skill(debug)` call re-pins fresh content.
          yield { type: "tool_call", id: "s2", name: "skill", arguments: { name: "debug" } }
          return
        }
        afterReactivation = context.systemKnowledge ?? ""
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(
      provider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "z".repeat(240))],
      { skillDir: await skillDir(), maxTokens: 480, maxTurns: 30, repeatFuse: false },
    )

    const text = await collectText(runner.run({ sessionId: "skill-deactivate", goal: "phase work" }))
    expect(text).toBe("done")

    expect(afterDeactivation).not.toContain(SKILL_BODY)
    expect(afterReactivation).toContain(SKILL_BODY)
  })

  it("skillLeaseTurns auto-deactivates after N turns", async () => {
    let call = 0
    let finalKnowledge = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
          return
        }
        if (call <= 10) {
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        finalKnowledge = context.systemKnowledge ?? ""
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(
      provider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "z".repeat(240))],
      {
        skillDir: await skillDir(),
        maxTokens: 480,
        maxTurns: 30,
        repeatFuse: false,
        skillLeaseTurns: 2,
      },
    )

    await collectText(runner.run({ sessionId: "skill-lease", goal: "leased skill" }))

    // The 2-turn lease expired long before the final turn, and the boundary sweeps that the
    // filler turns forced have dropped the pin.
    expect(finalKnowledge).not.toContain(SKILL_BODY)
  })
})
