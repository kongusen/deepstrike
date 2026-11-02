/**
 * Plan task 6.2 · a dynamic workflow program shares state with its agents through the blackboard:
 * each entry is a kernel item every later agent starts with, whatever its context inheritance, and
 * a phase-scoped entry leaves when its phase ends.
 */
import { RuntimeRunner, InMemorySessionLog } from "../src/advanced/public.js"

describe("dynamic workflow blackboard", () => {
  it("hands entries to the agents spawned after the write, by scope", async () => {
    const seen: Record<string, string[]> = {}
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: {
        async run(ctx: { spec: { goal: string }; manifest: { agent_id: string }; inheritedKnowledge?: Array<{ key?: string; content: string }> }) {
          seen[ctx.spec.goal.split("\n")[0]] = (ctx.inheritedKnowledge ?? []).map(entry => `${entry.key}=${entry.content}`)
          return {
            agentId: ctx.manifest.agent_id,
            result: { termination: "completed", finalMessage: { role: "assistant", content: "ok", toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
          }
        },
      },
    } as never)

    const run = await runner.runDynamicWorkflow(async ctx => {
      await ctx.agent("before any entry")
      await ctx.blackboard.put("criteria", "primary sources only")
      await ctx.phase("draft", async () => {
        await ctx.blackboard.put("outline", { sections: 3 }, { scope: "phase" })
        await ctx.agent("inside the phase")
      })
      await ctx.agent("after the phase")
      return ctx.blackboard.get("outline") ?? ctx.blackboard.get("criteria")
    }, { sessionId: "blackboard" })

    expect(run.value).toBe("primary sources only")
    expect(seen["before any entry"]).toEqual([])
    expect(seen["inside the phase"]).toEqual([
      "blackboard:criteria=primary sources only",
      'blackboard:outline={"sections":3}',
    ])
    expect(seen["after the phase"]).toEqual(["blackboard:criteria=primary sources only"])
  })
})
