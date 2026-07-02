/**
 * Strict dynamic context control: a loaded SKILL is method/procedural content reused for the rest
 * of the run, so its text gets pinned into the durable `knowledge` slot (rendered as
 * `systemKnowledge`) in addition to the ordinary tool_result already headed for `history`. Contrast
 * with `mm-paging-integration.test.ts`, which proves a `memory`-tool hit (single-use retrieval
 * content) does NOT get this treatment — it only ever lives in history and decays with it.
 */
import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const SKILL_BODY = "Debug guidance: always reproduce before fixing."

describe("skill content is pinned into durable knowledge on activation", () => {
  it("stays in systemKnowledge across later turns, and is pushed only once", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-knowledge-pin-"))
    await writeFile(join(dir, "debug.md"), `---\nname: debug\ndescription: Debug helper\n---\n${SKILL_BODY}`)

    let call = 0
    const knowledgeSnapshots: string[] = []
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        knowledgeSnapshots.push(context.systemKnowledge ?? "")
        if (call === 1) {
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
          return
        }
        if (call === 2) {
          // Load the same skill again — must not duplicate the knowledge entry.
          yield { type: "tool_call", id: "s2", name: "skill", arguments: { name: "debug" } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(provider, [], { skillDir: dir, maxTurns: 6 })
    await collectText(runner.run({ sessionId: "knowledge-pin", goal: "debug it" }))

    expect(call).toBeGreaterThanOrEqual(3)
    // Turn 1 (the load turn): not yet pushed.
    expect(knowledgeSnapshots[0]).not.toContain(SKILL_BODY)
    // Turn 3+ (after activation): the skill's content is durably present.
    expect(knowledgeSnapshots[knowledgeSnapshots.length - 1]).toContain(SKILL_BODY)
    // Exactly one copy — the second `skill(debug)` call must not duplicate the knowledge entry.
    const last = knowledgeSnapshots[knowledgeSnapshots.length - 1]
    const occurrences = last.split(SKILL_BODY).length - 1
    expect(occurrences).toBe(1)
  })
})
