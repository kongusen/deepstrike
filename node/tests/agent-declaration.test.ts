import { createAgent } from "../src/index.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"
import { WorkingMemory } from "../src/memory/working.js"
import { tool } from "../src/tools/index.js"

class CapturingProvider extends ReplayProvider {
  readonly requests: Parameters<ReplayProvider["stream"]>[] = []
  override async *stream(...args: Parameters<ReplayProvider["stream"]>) {
    this.requests.push(args)
    yield* super.stream(...args)
  }
}

it("snapshots declaration data before the caller mutates it", async () => {
  const provider = new CapturingProvider([{ role: "assistant", content: '{"answer":"ok"}' }], { wrap: true })
  const lookup = tool("lookup", "original", { type: "object", properties: {} }, () => "found")
  const providerOptions = { openai: { temperature: 0 } }
  const guardrails = [{ name: "deny-blocked", policy: { vetoes: ["blocked"] } }]
  const outputSchema = { type: "object", properties: { answer: { type: "string" } }, required: ["answer"] }
  const agent = createAgent({ tools: [lookup], providerOptions, guardrails, outputSchema, runtimeBinding: { provider } })

  lookup.schema.name = "changed"
  providerOptions.openai.temperature = 1
  guardrails[0].policy.vetoes.push("lookup")
  outputSchema.properties.answer.type = "number"

  for (let run = 0; run < 2; run++) {
    await expect(agent.run("go")).resolves.toMatchObject({ status: "completed", outputValidation: { ok: true } })
  }
  expect(provider.requests.map(([, schemas]) => schemas.map(schema => schema.name))).toEqual([["lookup"], ["lookup"]])
  expect(provider.requests.map(([, , extensions]) => extensions)).toEqual([
    { openai: { temperature: 0 } }, { openai: { temperature: 0 } },
  ])
  expect(Object.isFrozen(lookup)).toBe(false)
  expect(Object.isFrozen(providerOptions.openai)).toBe(false)
})

it("separates serializable declaration data from live services and executable tools", () => {
  const provider = new CapturingProvider([{ role: "assistant", content: "done" }])
  const memory = new WorkingMemory()
  memory.set("private-state", "not a declaration")
  const retriever = { async init() {}, async retrieve() { return ["fact"] } }
  const lookup = tool("lookup", "Lookup", { type: "object", properties: {} }, () => "ok")
  const agent = createAgent({
    tools: [lookup], memory,
    knowledge: [{ id: "vector", source: { kind: "vector", retriever } }],
    runtimeBinding: { provider },
    memoryStore: new InMemoryMemoryStore(), memoryScope: { tenant_id: "tenant", namespace: "notes" },
  })

  expect(agent.declaration).toMatchObject({ name: "agent", memory: { kind: "working" }, tools: [{ schema: { name: "lookup" } }] })
  expect(JSON.parse(JSON.stringify(agent.declaration))).toEqual(agent.declaration)
  expect(agent.declaration).not.toHaveProperty("runtimeBinding")
  expect(agent.declaration).not.toHaveProperty("memoryStore")
  expect(agent.declaration.tools?.[0]).not.toHaveProperty("execute")
  expect(agent.declaration.knowledge?.[0].source).toEqual({ kind: "vector" })
  expect(Object.isFrozen(agent.declaration.tools?.[0].schema)).toBe(true)
  expect(Object.isFrozen(provider)).toBe(false)
  expect(Object.isFrozen(memory)).toBe(false)
  expect(Object.isFrozen(retriever)).toBe(false)
})

it("keeps the selected provider and tool implementation when the input binding is reassigned", async () => {
  const provider = new CapturingProvider([
    { role: "assistant", content: "", toolCalls: [{ id: "lookup-1", name: "lookup", arguments: "{}" }] },
    { role: "assistant", content: "done" },
  ])
  const replacement = new CapturingProvider([{ role: "assistant", content: "wrong" }])
  let calls = 0
  const lookup = tool("lookup", "Lookup", { type: "object", properties: {} }, () => { calls++; return "ok" })
  const binding = { provider }
  const agent = createAgent({ tools: [lookup], runtimeBinding: binding })
  binding.provider = replacement
  lookup.execute = async () => "replaced"

  await expect(agent.run("go")).resolves.toMatchObject({ status: "completed", output: "done" })

  expect(calls).toBe(1)
  expect(provider.consumed()).toBe(2)
  expect(replacement.consumed()).toBe(0)
})
