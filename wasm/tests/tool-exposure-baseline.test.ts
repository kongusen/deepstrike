import { RuntimeRunner, collectText, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"

/**
 * WASM lowering for the exposure baseline (`baselineToolIds` → `run_spec.exposure_baseline`) and the
 * fail-closed dispatch selector (`toolDispatchGate` → `configure_run.tool_dispatch_gate`).
 *
 * The wasm suite runs against a MOCK kernel, so it cannot observe gate BEHAVIOR — that lives in the
 * kernel's own tests and in the node/python integration suites, which drive the real kernel. What is
 * genuinely wasm-side, and therefore what these tests pin, is the payload: the two additive fields
 * must reach the kernel with the right names and the right presence semantics (an empty baseline is
 * a real value, not an "unset" sentinel).
 */
const echoProvider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "ok", toolCalls: [] }
  },
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "ok" }
  },
}

async function runWith(opts: Record<string, unknown>) {
  kernelEvents.length = 0
  const runner = new RuntimeRunner({
    provider: echoProvider,
    sessionLog: new InMemorySessionLog(),
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 2048,
    ...opts,
  } as never)
  await collectText(runner.run({ sessionId: "exposure-wasm", goal: "go" }))
  const configure = kernelEvents.find(e => e.kind === "configure_run") as
    | { config: Record<string, unknown> }
    | undefined
  const start = kernelEvents.find(e => e.kind === "start_run") as
    | { run_spec?: Record<string, unknown> }
    | undefined
  return { configure, start }
}

describe("wasm lowering: exposure baseline + dispatch gate", () => {
  it("rides baselineToolIds on run_spec.exposure_baseline under the allowedToolIds ceiling", async () => {
    const { start } = await runWith({
      allowedToolIds: ["read", "write"],
      baselineToolIds: ["read"],
    })
    expect(start?.run_spec).toBeDefined()
    expect(start!.run_spec!.exposure_baseline).toEqual(["read"])
    expect(start!.run_spec!.capability_filter).toEqual({ allowed_kinds: [], allowed_ids: ["read", "write"] })
  })

  it("sends an EMPTY baseline as [] (the minimal surface), not as unset", async () => {
    // The presence semantics differ from `allowedToolIds` on purpose: `[]` there means "no gating",
    // here it means "meta-tools + stable-core only". A `length > 0` trigger would erase that.
    const { start } = await runWith({ baselineToolIds: [] })
    expect(start?.run_spec).toBeDefined()
    expect(start!.run_spec!.exposure_baseline).toEqual([])
  })

  it("omits exposure_baseline entirely when unset (no config = old behavior)", async () => {
    const { start } = await runWith({ allowedToolIds: ["read"] })
    expect(start?.run_spec).toBeDefined()
    expect(start!.run_spec).not.toHaveProperty("exposure_baseline")
  })

  it("lowers toolDispatchGate onto configure_run.tool_dispatch_gate", async () => {
    const { configure } = await runWith({ toolDispatchGate: "registered" })
    expect(configure).toBeDefined()
    expect(configure!.config.tool_dispatch_gate).toBe("registered")

    const exposed = await runWith({ toolDispatchGate: "exposed" })
    expect(exposed.configure!.config.tool_dispatch_gate).toBe("exposed")
  })

  it("omits tool_dispatch_gate when unset (kernel default stands)", async () => {
    const { configure } = await runWith({})
    expect(configure).toBeDefined()
    expect(configure!.config).not.toHaveProperty("tool_dispatch_gate")
  })
})
