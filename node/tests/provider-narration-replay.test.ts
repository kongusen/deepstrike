import { createAgent } from "../src/agent-facade.js"
import { deepseek } from "../src/providers/factories.js"
import { tool } from "../src/tools/index.js"

/** One streamed DeepSeek turn: reasoning, optional narration, then a tool call or a final answer. */
function turn(index: number, narration: string, final = false, reasoning = true): Record<string, unknown>[] {
  return [
    ...(reasoning ? [{ id: `r${index}`, choices: [{ delta: { reasoning_content: `thinking ${index}` } }] }] : []),
    ...(narration ? [{ choices: [{ delta: { content: narration } }] }] : []),
    ...(final
      ? [{ choices: [{ delta: {}, finish_reason: "stop" }] }]
      : [
          { choices: [{ delta: { tool_calls: [{ index: 0, id: `call_${index}`, function: { name: "probe", arguments: `{"n":${index}}` } }] } }] },
          { choices: [{ delta: {}, finish_reason: "tool_calls" }] },
        ]),
    { choices: [], usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 } },
  ]
}

// The kernel collapses an old turn's narration to a stub at render time; the reasoning streamed
// with that turn must still be found and replayed, or DeepSeek's replay check fails the run. Since
// plan task 8.3 narration collapses only behind a compaction boundary, so the run is long enough,
// and its tool results large enough, for the window to compact.
test("a narrated tool turn keeps its reasoning after the kernel collapses the narration", async () => {
  const provider = deepseek({ apiKey: "k", model: "deepseek-flash" })
  const TURNS = 16
  const narrated = (i: number) => i >= 3 && i <= 8
  const turns = [
    ...Array.from({ length: TURNS }, (_, i) =>
      turn(i, narrated(i) ? `Step ${i}: let me probe the next value so I can see what it returns.` : "")),
    turn(TURNS, "done", true),
  ]
  const collapsed: Array<{ call: string; reasoning: unknown }> = []
  let call = 0
  ;(provider as any).client = {
    chat: { completions: { create: async (params: { messages: Array<Record<string, unknown>> }) => {
      for (const message of params.messages) {
        if (message.role === "assistant" && String(message.content).startsWith("[earlier narration collapsed")) {
          const calls = message.tool_calls as Array<{ id: string }> | undefined
          collapsed.push({ call: calls?.[0]?.id ?? "", reasoning: message.reasoning_content })
        }
      }
      const chunks = turns[call++]
      return (async function* () { yield* chunks })()
    } } },
  }
  const probe = tool("probe", "probe", { type: "object", properties: { n: { type: "number" } } }, async () => "value ".repeat(600))
  const agent = createAgent({
    name: "a", model: "deepseek-flash", tools: [probe], maxTokens: 12_000,
    runtimeBinding: { providerFor: () => provider },
  })

  await expect(agent.run("probe many times")).resolves.toMatchObject({ status: "completed" })
  expect(collapsed.length).toBeGreaterThan(0)
  // Each collapsed turn replays the reasoning streamed with that very turn.
  expect(collapsed.every(entry => entry.reasoning === `thinking ${entry.call.replace("call_", "")}`)).toBe(true)
})
