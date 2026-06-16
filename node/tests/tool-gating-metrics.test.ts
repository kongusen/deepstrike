import { mkdtemp, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { TurnMetrics } from "../src/runtime/runner.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

/**
 * P0-C tool-gating telemetry: `onTurnMetrics` must surface, per LLM turn, the data the epoch-gating
 * go/no-go analysis needs — exposure vs call counts, the prompt-cache split, and the active skill
 * (for dwell). Pure observation: it must never alter the run.
 */
describe("P0-C tool-gating telemetry (onTurnMetrics)", () => {
  it("reports exposure/call counts and the prompt-cache split", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done" }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield {
          type: "usage",
          totalTokens: 1050,
          inputTokens: 1000,
          outputTokens: 50,
          cacheReadInputTokens: 900,
          cacheCreationInputTokens: 100,
        } as StreamEvent
        yield { type: "text_delta", delta: "done" }
      },
    }
    const metrics: TurnMetrics[] = []
    const { runner } = createRunner(
      provider,
      [
        tool("read", "read", { type: "object", properties: {} }, async () => "r"),
        tool("write", "write", { type: "object", properties: {} }, async () => "w"),
      ],
      { onTurnMetrics: m => metrics.push(m) },
    )
    await collectText(runner.run({ sessionId: "metrics-core", goal: "go" }))

    expect(metrics.length).toBeGreaterThanOrEqual(1)
    const m = metrics[0]
    expect(m.toolsExposed).toBe(2) // read + write, no meta-tools registered
    expect(m.toolsCalled).toBe(0)
    expect(m.inputTokens).toBe(1000)
    expect(m.cacheReadTokens).toBe(900)
    expect(m.cacheCreationTokens).toBe(100)
    expect(m.activeSkill).toBeUndefined()
  })

  it("tracks activeSkill across turns for dwell measurement", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-skill-"))
    await writeFile(
      join(dir, "debug.md"),
      "---\nname: debug\ndescription: Debug helper\n---\nDebug instructions.",
    )

    let call = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done" }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        call += 1
        yield { type: "usage", totalTokens: 110, inputTokens: 100, outputTokens: 10 } as StreamEvent
        if (call === 1) {
          // Turn 1 loads the skill; it only takes effect for the *next* reason turn.
          yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
        } else {
          yield { type: "text_delta", delta: "done" }
        }
      },
    }
    const metrics: TurnMetrics[] = []
    const { runner } = createRunner(provider, [], {
      skillDir: dir,
      onTurnMetrics: m => metrics.push(m),
    })
    await collectText(runner.run({ sessionId: "metrics-dwell", goal: "debug it" }))

    expect(metrics.length).toBeGreaterThanOrEqual(2)
    // Going into turn 1 no skill is active; after the turn-1 `skill` call, turn 2 sees it loaded.
    expect(metrics[0].activeSkill).toBeUndefined()
    expect(metrics[metrics.length - 1].activeSkill).toBe("debug")
  })

  it("a throwing sink never breaks the run", async () => {
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done" }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "done" }
      },
    }
    const { runner } = createRunner(provider, [], {
      onTurnMetrics: () => {
        throw new Error("sink boom")
      },
    })
    const text = await collectText(runner.run({ sessionId: "metrics-throw", goal: "go" }))
    expect(text).toContain("done")
  })
})
