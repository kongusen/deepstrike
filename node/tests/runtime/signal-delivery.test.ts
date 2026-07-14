import {
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
} from "../../src/index.js"
import type {
  LeasedSignalSource,
  LLMProvider,
  Message,
  RenderedContext,
  RuntimeSignal,
  SignalClaim,
  SignalDeliveryReceipt,
  StreamEvent,
  ToolSchema,
} from "../../src/index.js"

class TextProvider implements LLMProvider {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }
  async *stream(_context: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "done" }
  }
}

class RecordingLeasedSource implements LeasedSignalSource {
  readonly acked: SignalDeliveryReceipt[] = []
  readonly nacked: SignalDeliveryReceipt[] = []
  private claimed = false

  constructor(private readonly ackSucceeds = true) {}

  async nextSignal(): Promise<RuntimeSignal | null> {
    throw new Error("legacy destructive pull must not be used")
  }

  async claimSignal(): Promise<SignalClaim | null> {
    if (this.claimed) return null
    this.claimed = true
    return {
      deliveryId: "delivery-1",
      leaseToken: "lease-1",
      leaseExpiresAtMs: Date.now() + 30_000,
      signal: {
        source: "gateway",
        signalType: "event",
        urgency: "normal",
        payload: { goal: "leased" },
      },
    }
  }

  async ackSignal(receipt: SignalDeliveryReceipt): Promise<boolean> {
    this.acked.push(receipt)
    return this.ackSucceeds
  }

  async nackSignal(receipt: SignalDeliveryReceipt): Promise<boolean> {
    this.nacked.push(receipt)
    return true
  }
}

function runnerWith(source: LeasedSignalSource): RuntimeRunner {
  return new RuntimeRunner({
    provider: new TextProvider(),
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    signalSource: source,
    maxTokens: 2048,
    maxTurns: 2,
  })
}

describe("RuntimeRunner leased signal delivery", () => {
  it("acks only after the kernel accepts a claimed signal", async () => {
    const source = new RecordingLeasedSource()

    for await (const _event of runnerWith(source).run({ sessionId: "leased", goal: "work" })) { /* drain */ }

    expect(source.acked).toHaveLength(1)
    expect(source.nacked).toHaveLength(0)
  })

  it("nacks and surfaces an error when acknowledgement loses the lease", async () => {
    const source = new RecordingLeasedSource(false)
    const events = []

    for await (const event of runnerWith(source).run({ sessionId: "lease-lost", goal: "work" })) {
      events.push(event)
    }

    expect(source.acked).toHaveLength(1)
    expect(source.nacked).toHaveLength(1)
    expect(events.some(event => event.type === "error" && event.message.includes("signal lease"))).toBe(true)
  })
})
