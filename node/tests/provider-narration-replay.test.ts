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
// with that turn must still be found and replayed, or DeepSeek's replay check fails the run.
test("a narrated tool turn keeps its reasoning after the kernel collapses the narration", async () => {
  const provider = deepseek({ apiKey: "k", model: "deepseek-flash" })
  const turns = [
    turn(0, "Let me start by probing the first value so I can see what it returns."),
    ...Array.from({ length: 4 }, (_, i) => turn(i + 1, "")),
    turn(5, "done", true),
  ]
  const replayed: Array<{ content: string; reasoning: unknown }> = []
  let call = 0
  ;(provider as any).client = {
    chat: { completions: { create: async (params: { messages: Array<Record<string, unknown>> }) => {
      const first = params.messages.find(message => message.role === "assistant")
      if (first) replayed.push({ content: String(first.content), reasoning: first.reasoning_content })
      const chunks = turns[call++]
      return (async function* () { yield* chunks })()
    } } },
  }
  const probe = tool("probe", "probe", { type: "object", properties: { n: { type: "number" } } }, async () => "ok")
  const agent = createAgent({ name: "a", model: "deepseek-flash", tools: [probe], runtimeBinding: { providerFor: () => provider } })

  await expect(agent.run("probe five times")).resolves.toMatchObject({ status: "completed" })
  const collapsed = replayed.filter(entry => entry.content.startsWith("[earlier narration collapsed"))
  expect(collapsed.length).toBeGreaterThan(0)
  expect(collapsed.every(entry => entry.reasoning === "thinking 0")).toBe(true)
})

// DeepSeek sometimes answers a tool-call turn without any reasoning. Thinking mode requires the
// field on every replayed tool-call turn and accepts it empty, so the turn replays it empty.
test("a tool turn the model answered without reasoning replays empty reasoning", async () => {
  const provider = deepseek({ apiKey: "k", model: "deepseek-flash" })
  const turns = [turn(0, "Checking.", false, false), turn(1, "done", true)]
  const replayed: unknown[] = []
  let call = 0
  ;(provider as any).client = {
    chat: { completions: { create: async (params: { messages: Array<Record<string, unknown>> }) => {
      const first = params.messages.find(message => message.role === "assistant")
      if (first) replayed.push(first.reasoning_content)
      const chunks = turns[call++]
      return (async function* () { yield* chunks })()
    } } },
  }
  const probe = tool("probe", "probe", { type: "object", properties: { n: { type: "number" } } }, async () => "ok")
  const agent = createAgent({ name: "a", model: "deepseek-flash", tools: [probe], runtimeBinding: { providerFor: () => provider } })

  await expect(agent.run("probe once")).resolves.toMatchObject({ status: "completed" })
  expect(replayed).toEqual([""])
})
