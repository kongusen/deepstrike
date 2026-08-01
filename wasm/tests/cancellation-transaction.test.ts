import {
  DriverKernelJournal,
  InMemoryJournalDriver,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
} from "../src/runtime/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

describe("kernel cancellation transaction", () => {
  it.each(["user", "deadline", "lease_lost", "host_shutdown"] as const)(
    "commits the %s reason with its pending provider call",
    async reason => {
      const provider: LLMProvider = {
        async complete(): Promise<Message> {
          return { role: "assistant", content: "", toolCalls: [] }
        },
        async *stream(): AsyncIterable<StreamEvent> {
          yield { type: "text_delta", delta: "first" }
          for (let i = 0; i < 1000; i += 1) yield { type: "text_delta", delta: "later" }
        },
      }
      const sessionLog = new InMemorySessionLog()
      const runner = new RuntimeRunner({ provider, sessionLog, executionPlane: new LocalExecutionPlane(), maxTokens: 2048 })

      for await (const event of runner.run({ sessionId: `cancel-${reason}`, goal: "cancel me" })) {
        if (event.type === "text_delta") runner.interrupt(reason)
      }

      const cancellation = (await sessionLog.read(`cancel-${reason}`))
        .map(entry => entry.event)
        .find(event => event.kind === "operation_cancelled")
      expect(cancellation).toMatchObject({ kind: "operation_cancelled", reason })
      expect(cancellation && "pending_call_ids" in cancellation ? cancellation.pending_call_ids : []).toHaveLength(1)
    },
  )

  it("aborts provider I/O and lets a fresh worker observe the terminal journal", async () => {
    const driver = new InMemoryJournalDriver()
    const journal = new DriverKernelJournal(driver)
    const sessionLog = new InMemorySessionLog()
    let receivedSignal: AbortSignal | undefined
    let providerClosed = false
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(_context, _tools, _extensions, _state, signal): AsyncIterable<StreamEvent> {
        receivedSignal = signal
        try {
          yield { type: "text_delta", delta: "first" }
          while (!signal?.aborted) {
            await Promise.resolve()
            yield { type: "text_delta", delta: "later" }
          }
        } finally {
          providerClosed = true
        }
      },
    }
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      kernelJournal: journal,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })

    for await (const event of runner.run({ sessionId: "worker-cancel", goal: "cancel me" })) {
      if (event.type === "text_delta") runner.interrupt("host_shutdown")
    }

    expect(receivedSignal?.aborted).toBe(true)
    expect(receivedSignal?.reason).toBe("host_shutdown")
    expect(providerClosed).toBe(true)

    const started = (await sessionLog.read("worker-cancel"))
      .find(entry => entry.event.kind === "run_started")?.event
    if (!started || started.kind !== "run_started") throw new Error("missing run_started")
    const reopenedJournal = new DriverKernelJournal(driver)
    expect(await reopenedJournal.head(`wasm-operation-${started.run_id}`)).toBeDefined()

    let resumedProviderCalls = 0
    const restarted = new RuntimeRunner({
      provider: {
        async complete(): Promise<Message> {
          resumedProviderCalls += 1
          return { role: "assistant", content: "unexpected", toolCalls: [] }
        },
        async *stream(): AsyncIterable<StreamEvent> {
          resumedProviderCalls += 1
          yield { type: "text_delta", delta: "unexpected" }
        },
      },
      sessionLog,
      kernelJournal: reopenedJournal,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
    })

    const resumedEvents: StreamEvent[] = []
    for await (const event of restarted.wake("worker-cancel")) resumedEvents.push(event)
    expect(resumedEvents).toEqual([])
    expect(resumedProviderCalls).toBe(0)
  })
})
