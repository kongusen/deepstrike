import { CanonicalKernel } from "@deepstrike/wasm-kernel"
import {
  CanonicalKernelHost,
  CanonicalRunnerRuntime,
} from "../src/runtime/canonical-kernel-step.js"
import { InMemoryKernelJournal } from "../src/runtime/kernel-journal.js"
import {
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
} from "../src/runtime/index.js"
import { tool } from "../src/tools/index.js"

/**
 * Durable restart recovery over the **InMemory ladder** (0.2.65 S2, Q3 adjudication): the
 * browser target has no File layer — host persistence is the host's job (B7) — so the wasm
 * proof keeps the journal instance as the durable artifact and treats a fresh `CanonicalKernel`
 * rebuilt from it as the restarted process. Both restore ladders are exercised — journal-only
 * replay, and checkpoint+tail after the install/ack/reclaim boundary. The crash-window
 * projections (CAS conflict, staged-envelope drain) are proven end to end by the node/python
 * FileKernelJournal suites; the jest kernel is the mock, which models but does not reimplement
 * the real prepare/commit contract.
 *
 * Equivalence criterion: the recovered run's next step is the step the original input
 * determined — the pending effect survives the restart at its exact chain position — and the
 * run reaches the same terminal an uninterrupted twin reaches, with each effect executed
 * exactly once across the restart.
 */

const RUN_ID = "restart-op-1"
const OP = `wasm-operation-${RUN_ID}`
const SESSION = "durable-restart"
const FINAL_TEXT = "restart-equivalent-finish"
const RUNTIME_OPTIONS = { maxContextTokens: 8_192, maxTurns: 8 }

/**
 * Streams the ping tool call until the effect has executed, then the final text. The mock
 * kernel's provider context is constant, so "has the effect run?" is observed through the
 * shared execution counter — which is also what makes exactly-once assertable: a re-emitted
 * tool call after recovery would push the count past one and redden the test.
 */
function providerFor(executions: { count: number }) {
  return {
    async complete(): Promise<never> {
      throw new Error("unused")
    },
    async *stream(): AsyncIterable<{ type: string; delta?: string; id?: string; name?: string; arguments?: Record<string, unknown> }> {
      if (executions.count === 0) {
        yield { type: "tool_call", id: "call_ping", name: "ping", arguments: {} }
        return
      }
      yield { type: "text_delta", delta: FINAL_TEXT }
    },
  }
}

function newRuntime(journal: InMemoryKernelJournal): CanonicalRunnerRuntime {
  return new CanonicalRunnerRuntime(new CanonicalKernel(), journal, OP, RUNTIME_OPTIONS)
}

function runnerFor(log: InMemorySessionLog, journal: InMemoryKernelJournal, executions: { count: number }): RuntimeRunner {
  const plane = new LocalExecutionPlane()
  plane.register(tool("ping", "Ping", { type: "object", properties: {} }, () => {
    executions.count += 1
    return "pong"
  }))
  return new RuntimeRunner({
    provider: providerFor(executions),
    sessionLog: log,
    kernelJournal: journal,
    executionPlane: plane,
    maxTokens: RUNTIME_OPTIONS.maxContextTokens,
    maxTurns: RUNTIME_OPTIONS.maxTurns,
    baselineToolIds: ["ping"],
  })
}

/**
 * Drive the operation to a pending `execute_tool` effect — the freeze frame of a run
 * interrupted between committing the provider turn and executing the requested tool.
 * Tool calls are keyed `call_id` — the canonical field the host forwards to the kernel.
 */
async function driveToPendingToolEffect(runtime: CanonicalRunnerRuntime): Promise<void> {
  await runtime.applyHostEvent({
    kind: "set_tools",
    tools: [{ name: "ping", description: "Ping", parameters: { type: "object", properties: {} } }],
  })
  const first = await runtime.startAgent(
    { goal: "use ping then finish", criteria: [] },
    { exposure_baseline: ["ping"] },
  )
  if (first?.kind !== "call_provider") throw new Error(`expected call_provider, got ${String(first?.kind)}`)
  const pending = await runtime.applyHostEvent({
    kind: "provider_result",
    effect_id: first.effectId,
    message: {
      role: "assistant",
      content: "",
      tool_calls: [{ call_id: "call_ping", name: "ping", arguments: {} }],
    },
    stop_reason: "tool_use",
  })
  if (pending?.kind !== "execute_tool") throw new Error(`expected execute_tool, got ${String(pending?.kind)}`)
}

/** The journal chain as (step_seq, digest, base64 bytes) triples — bytes are the comparison unit. */
async function chain(journal: InMemoryKernelJournal): Promise<Array<{ step_seq: number; digest: string; bytes: string }>> {
  return (await journal.readFrom(OP)).map(entry => ({
    step_seq: entry.step_seq,
    digest: entry.record_digest,
    bytes: Buffer.from(entry.record_bytes).toString("base64"),
  }))
}

describe("durable restart recovery — wasm InMemory ladder (0.2.65 S2)", () => {
  it("an uninterrupted run fixes the terminal every restart must reproduce", async () => {
    const log = new InMemorySessionLog()
    const executions = { count: 0 }
    const runner = runnerFor(log, log.kernelJournal as InMemoryKernelJournal, executions)
    const text = await collectText(runner.run({ sessionId: SESSION, goal: "use ping then finish" }))
    expect(text).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
  })

  it("journal-only ladder: a crash after commit before the effect resumes from journal bytes alone", async () => {
    const log = new InMemorySessionLog()
    await log.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    const journal = log.kernelJournal as InMemoryKernelJournal
    await driveToPendingToolEffect(newRuntime(journal))
    const frozen = await chain(journal)
    expect(frozen.length).toBeGreaterThanOrEqual(3)

    // Process restart: the kernel instance dies; the journal is the durable artifact. The
    // frozen prefix read back must be byte-identical — the bytes are the whole contract.
    expect(await chain(journal)).toEqual(frozen)

    const executions = { count: 0 }
    // runner.wake builds a fresh kernel over the journal and resumes the pending effect.
    expect(await collectText(runnerFor(log, journal, executions).wake(SESSION))).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
    const resumed = await chain(journal)
    expect(resumed.slice(0, frozen.length)).toEqual(frozen)
    expect(resumed.length).toBeGreaterThan(frozen.length)
  })

  it("checkpoint+tail ladder: a crash after install/ack/reclaim restores through the checkpoint", async () => {
    const log = new InMemorySessionLog()
    await log.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    const journal = log.kernelJournal as InMemoryKernelJournal
    const kernel = new CanonicalKernel()
    const runtime = new CanonicalRunnerRuntime(kernel, journal, OP, RUNTIME_OPTIONS)
    await driveToPendingToolEffect(runtime)
    const frozen = await chain(journal)

    // The §12.3 boundary — install, ack, reclaim — runs before the process dies. The pending
    // tool effect lives inside the checkpoint; the covered prefix is reclaimed.
    const installed = await new CanonicalKernelHost(kernel, journal, OP).checkpoint()
    expect(installed.acknowledged).toBe(true)
    const retained = await chain(journal)
    expect(retained.length).toBeLessThan(frozen.length)

    expect((await journal.latestCheckpoint(OP))?.acknowledged).toBe(true)
    expect(await chain(journal)).toEqual(retained)

    // The wake restore takes the checkpoint+tail ladder: latestCheckpoint + recordsAfter(
    // covered_head). The reclaimed prefix never returns; the run continues past it.
    const executions = { count: 0 }
    expect(await collectText(runnerFor(log, journal, executions).wake(SESSION))).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
    const resumed = await chain(journal)
    expect(resumed.slice(0, retained.length)).toEqual(retained)
    expect(resumed.every(entry => entry.step_seq > installed.through_step_seq)).toBe(true)
  })
})
