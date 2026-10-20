import { createServer, type Server } from "node:http"
import { createProvider } from "../../src/index.js"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { InMemoryKernelJournal, type JournalAppendReceipt, type JournalRecordInput } from "../../src/runtime/kernel-journal.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool } from "../../src/tools/index.js"
import { getKernel } from "../../src/kernel.js"
import { CanonicalRunnerRuntime } from "../../src/runtime/canonical-kernel-step.js"
import { messageToKernelMessage } from "../../src/runtime/kernel-step.js"

describe("interrupted history at a fresh operation boundary", () => {
  let server: Server
  let baseURL: string
  let requests: Array<{ messages: Array<Record<string, unknown>> }>
  let toolTurns: number

  beforeAll(async () => {
    server = createServer((req, res) => {
      let raw = ""
      req.on("data", chunk => { raw += chunk })
      req.on("end", () => {
        requests.push(JSON.parse(raw))
        res.writeHead(200, { "content-type": "text/event-stream" })
        const n = requests.length
        const delta = n <= toolTurns
          ? { role: "assistant", tool_calls: [{ index: 0, id: `call_${n}`, type: "function", function: { name: "read", arguments: JSON.stringify({ page: n }) } }] }
          : { role: "assistant", content: "Done." }
        res.end(`data: ${JSON.stringify({ id: "c", choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`
          + `data: ${JSON.stringify({ id: "c", choices: [{ index: 0, delta: {}, finish_reason: n <= toolTurns ? "tool_calls" : "stop" }] })}\n\n`
          + `data: ${JSON.stringify({ id: "c", choices: [], usage: { prompt_tokens: 100, completion_tokens: 5, total_tokens: 105 } })}\n\ndata: [DONE]\n\n`)
      })
    })
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve))
    const address = server.address()
    baseURL = `http://127.0.0.1:${typeof address === "object" && address ? address.port : 0}/v1`
  })
  afterAll(() => new Promise<void>(resolve => server.close(() => resolve())))
  beforeEach(() => { requests = []; toolTurns = 0 })

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

  it("compacts large CJK history without renewing away the conversation", async () => {
    const sessionLog = new InMemorySessionLog()
    const paragraph = "需求分析与原型设计的详细说明，".repeat(80)
    for (let i = 0; i < 30; i++) {
      await sessionLog.append("pressure", { kind: "run_started", run_id: `old-${i}`, goal: `第${i}轮：${paragraph}`, criteria: [] })
      await sessionLog.append("pressure", { kind: "llm_completed", turn: 1, content: paragraph, tool_calls: [] })
      await sessionLog.append("pressure", { kind: "run_terminal", reason: "completed", turns_used: 1, total_tokens: 0 })
    }
    toolTurns = 4
    const read = tool("read", "Read", { type: "object", properties: { page: { type: "integer" } } }, () => "组件内容与评审意见，".repeat(200))
    const runner = new RuntimeRunner({
      provider: createProvider({ model: "m", apiKey: "test", baseURL, provider: "openai" }),
      executionPlane: new LocalExecutionPlane().register(read), sessionLog, kernelJournal: new InMemoryKernelJournal(),
      baselineToolIds: ["read"], maxTokens: 16_000, maxTurns: 8,
    })
    const errors = []
    for await (const event of runner.run({ sessionId: "pressure", goal: "读四页" })) if (event.type === "error") errors.push(event)
    expect(errors).toEqual([])
    expect(requests).toHaveLength(5)
    const events = (await sessionLog.read("pressure")).map(entry => entry.event.kind)
    expect(events).toContain("compressed")
    expect(events).not.toContain("context_renewed")
    expect(JSON.stringify(requests.at(-1)?.messages)).toMatch(/第29轮|Compressed/)
  }, 30_000)

  it("resumes a durable configuration interrupted before start_operation was staged", async () => {
    class CrashBeforeStartJournal extends InMemoryKernelJournal {
      crash = true
      override async stageOutboundEnvelope(operationId: string, inputJson: string): Promise<void> {
        if (this.crash && JSON.parse(inputJson).input.kind === "start_operation") {
          this.crash = false
          throw new Error("simulated worker loss before root start")
        }
        return super.stageOutboundEnvelope(operationId, inputJson)
      }
    }
    const journal = new CrashBeforeStartJournal()
    const sessionLog = new InMemorySessionLog()
    const options = {
      provider: createProvider({ model: "m", apiKey: "test", baseURL, provider: "openai" }),
      executionPlane: new LocalExecutionPlane(), sessionLog, kernelJournal: journal,
      maxTokens: 8000, maxTurns: 2,
    }
    await expect(collectText(new RuntimeRunner(options).run({ sessionId: "bootstrap", goal: "Original goal" }))).rejects.toThrow("simulated worker loss")
    const text = await collectText(new RuntimeRunner(options).run({ sessionId: "bootstrap", goal: "Retry" }))
    expect(text).toBe("Done.")
    expect(requests).toHaveLength(1)
    expect(JSON.stringify(requests[0].messages)).toContain("Original goal")
    expect((await sessionLog.read("bootstrap")).filter(entry => entry.event.kind === "run_started")).toHaveLength(1)
  })

  it("drains a staged root start without starting the operation twice", async () => {
    class CrashDuringStartJournal extends InMemoryKernelJournal {
      crash = true
      override async compareAndAppend(operationId: string, expectedHead: string | undefined, record: JournalRecordInput): Promise<JournalAppendReceipt> {
        if (this.crash && record.step_seq === 1) {
          this.crash = false
          throw new Error("simulated worker loss with staged root start")
        }
        return super.compareAndAppend(operationId, expectedHead, record)
      }
    }
    const journal = new CrashDuringStartJournal()
    const sessionLog = new InMemorySessionLog()
    const options = {
      provider: createProvider({ model: "m", apiKey: "test", baseURL, provider: "openai" }),
      executionPlane: new LocalExecutionPlane(), sessionLog, kernelJournal: journal,
      maxTokens: 8000, maxTurns: 2,
    }
    await expect(collectText(new RuntimeRunner(options).run({ sessionId: "staged-start", goal: "Original goal" }))).rejects.toThrow("simulated worker loss")
    expect(await collectText(new RuntimeRunner(options).run({ sessionId: "staged-start", goal: "Retry" }))).toBe("Done.")
    expect(requests).toHaveLength(1)
    expect(JSON.stringify(requests[0].messages)).toContain("Original goal")
  })

  it("recovers a session whose previous SDK persisted an invalid pending provider request", async () => {
    const sessionLog = new InMemorySessionLog()
    const journal = new InMemoryKernelJournal()
    await sessionLog.append("stuck", { kind: "run_started", run_id: "old", goal: "Read", criteria: [] })
    await sessionLog.append("stuck", { kind: "llm_completed", turn: 1, content: "", tool_calls: [{ id: "call_lost", name: "read", arguments: "{}" }] })
    await sessionLog.append("stuck", { kind: "run_terminal", reason: "cancelled", turns_used: 1, total_tokens: 0 })
    await sessionLog.append("stuck", { kind: "run_started", run_id: "broken", goal: "Continue", criteria: [] })
    const runtime = new CanonicalRunnerRuntime(new (getKernel().CanonicalKernel)(), journal, "node-operation-broken", { maxContextTokens: 8000 })
    await runtime.applyHostEvent({ kind: "preload_history", messages: [
      messageToKernelMessage({ role: "user", content: "Read", toolCalls: [] }),
      messageToKernelMessage({ role: "assistant", content: "", toolCalls: [{ id: "call_lost", name: "read", arguments: "{}" }] }),
    ] })
    await runtime.startAgent({ goal: "Continue", criteria: [] })
    await sessionLog.append("stuck", { kind: "run_terminal", reason: "error", turns_used: 0, total_tokens: 0 })
    const runner = new RuntimeRunner({
      provider: createProvider({ model: "m", apiKey: "test", baseURL, provider: "openai" }),
      executionPlane: new LocalExecutionPlane(), sessionLog, kernelJournal: journal,
      maxTokens: 8000, maxTurns: 2,
    })
    const events = []
    for await (const event of runner.run({ sessionId: "stuck", goal: "Recover" })) events.push(event)
    expect(events.filter(event => event.type === "error")).toEqual([])
    expect(requests).toHaveLength(1)
    expect(JSON.stringify(requests[0].messages)).toContain("Recover")
  })
})
