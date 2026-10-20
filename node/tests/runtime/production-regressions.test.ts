import { createServer, type Server } from "node:http"
import { createProvider } from "../../src/index.js"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { InMemoryKernelJournal } from "../../src/runtime/kernel-journal.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"

describe("interrupted history at a fresh operation boundary", () => {
  let server: Server
  let baseURL: string
  let requests: Array<{ messages: Array<Record<string, unknown>> }>

  beforeAll(async () => {
    server = createServer((req, res) => {
      let raw = ""
      req.on("data", chunk => { raw += chunk })
      req.on("end", () => {
        requests.push(JSON.parse(raw))
        res.writeHead(200, { "content-type": "text/event-stream" })
        res.end(`data: ${JSON.stringify({ id: "c", choices: [{ index: 0, delta: { role: "assistant", content: "Done." }, finish_reason: "stop" }], usage: { prompt_tokens: 100, completion_tokens: 5, total_tokens: 105 } })}\n\ndata: [DONE]\n\n`)
      })
    })
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve))
    const address = server.address()
    baseURL = `http://127.0.0.1:${typeof address === "object" && address ? address.port : 0}/v1`
  })
  afterAll(() => new Promise<void>(resolve => server.close(() => resolve())))
  beforeEach(() => { requests = [] })

  it.each([false, true])("answers abandoned calls with an error (inheritEvents=%s)", async inherited => {
    const sessionLog = new InMemorySessionLog()
    await sessionLog.append("parent", { kind: "run_started", run_id: "old", goal: "Read it", criteria: [] })
    await sessionLog.append("parent", { kind: "llm_completed", turn: 1, content: "", tool_calls: [{ id: "call_lost", name: "read", arguments: "{}" }] })
    if (!inherited) await sessionLog.append("parent", { kind: "run_terminal", reason: "cancelled", turns_used: 1, total_tokens: 0 })
    const runner = new RuntimeRunner({
      provider: createProvider({ model: "m", apiKey: "test", baseURL, provider: "openai" }),
      executionPlane: new LocalExecutionPlane(), sessionLog, kernelJournal: new InMemoryKernelJournal(),
      maxTokens: 8000, maxTurns: 2,
    })
    const events = []
    for await (const event of runner.run({ sessionId: inherited ? "child" : "parent", goal: "Continue", ...(inherited ? { inheritEvents: await sessionLog.read("parent") } : {}) })) events.push(event)
    expect(events.filter(event => event.type === "error")).toEqual([])
    expect(requests).toHaveLength(1)
    const result = requests[0].messages.find(message => message.tool_call_id === "call_lost")
    expect(result?.content).toMatch(/no result.*recorded/i)
    expect(result?.content).toMatch(/do not assume/i)
  })
})
