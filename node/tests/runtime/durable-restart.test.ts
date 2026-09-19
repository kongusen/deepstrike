import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { getKernel } from "../../src/kernel.js"
import {
  CanonicalKernelHost,
  CanonicalRunnerRuntime,
} from "../../src/runtime/canonical-kernel-step.js"
import type { KernelJournal } from "../../src/runtime/kernel-journal.js"
import { JournalCasConflictError, JournalIoError } from "../../src/runtime/kernel-journal.js"
import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { FileSessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool } from "../../src/tools/index.js"
import type { LLMProvider, RenderedContext, StreamEvent, ToolSchema, ProviderMessage } from "../../src/types.js"

/**
 * Durable restart recovery — the whole durable path against a **file-backed** journal
 * (0.2.65 S2). The cutover tests prove the canonical protocol over `InMemorySessionLog`;
 * these tests prove it survives a real remount: every phase reopens the directory with a
 * fresh `FileSessionLog`, the way a restarted process would. Both restore ladders are
 * exercised — journal-only replay, and checkpoint+tail after the install/ack/reclaim
 * boundary — plus the two crash windows the host protocol owes byte-identical retries:
 * a CAS conflict during append (crash-point #8: rebuild, then replay the same bytes) and
 * a crash between staging the outbound envelope and its append-ack (drained on wake,
 * adjudication 5e.3).
 *
 * Equivalence criterion: the recovered run's next step is the step the original input
 * determined — the committed record embeds exactly the staged envelope bytes — and the
 * run reaches the same terminal an uninterrupted twin reaches, with each effect executed
 * exactly once across the restart.
 */

const RUN_ID = "restart-op-1"
const OP = `node-operation-${RUN_ID}`
const SESSION = "durable-restart"
const FINAL_TEXT = "restart-equivalent-finish"

/** Streams the ping tool call until history holds a tool result, then the final text. */
class PingThenFinishProvider implements LLMProvider {
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<ProviderMessage> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    if (context.turns.some(turn => turn.role === "tool")) {
      yield { type: "text_delta", delta: FINAL_TEXT }
      return
    }
    yield { type: "tool_call", id: "call_ping", name: "ping", arguments: {} }
  }
}

const RUNTIME_OPTIONS = { maxContextTokens: 8_000, maxTurns: 8 } as const

function newRuntime(journal: KernelJournal): CanonicalRunnerRuntime {
  return new CanonicalRunnerRuntime(new (getKernel().CanonicalKernel)(), journal, OP, RUNTIME_OPTIONS)
}

function runnerFor(log: FileSessionLog, executions: { count: number }): RuntimeRunner {
  const plane = new LocalExecutionPlane()
  plane.register(tool("ping", "Ping", { type: "object", properties: {} }, () => {
    executions.count += 1
    return "pong"
  }))
  return new RuntimeRunner({
    provider: new PingThenFinishProvider(),
    sessionLog: log,
    executionPlane: plane,
    maxTokens: RUNTIME_OPTIONS.maxContextTokens,
    maxTurns: RUNTIME_OPTIONS.maxTurns,
    // The manual drives start with `exposure_baseline: ["ping"]`; the runner-level run()
    // needs the same surface or the tool call is denied instead of executed.
    baselineToolIds: ["ping"],
  })
}

/**
 * Drive the operation to a pending `execute_tool` effect — the freeze frame of a run
 * interrupted between committing the provider turn and executing the requested tool.
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
      tool_calls: [{ id: "call_ping", name: "ping", arguments: {} }],
    },
    stop_reason: "tool_use",
  })
  if (pending?.kind !== "execute_tool") throw new Error(`expected execute_tool, got ${String(pending?.kind)}`)
}

/** The journal chain as (step_seq, digest, base64 bytes) triples — bytes are the comparison unit. */
async function chain(journal: KernelJournal): Promise<Array<{ step_seq: number; digest: string; bytes: string }>> {
  return (await journal.readFrom(OP)).map(entry => ({
    step_seq: entry.step_seq,
    digest: entry.record_digest,
    bytes: Buffer.from(entry.record_bytes).toString("base64"),
  }))
}

/**
 * A journal decorator that records every staged outbound envelope and fails the Nth append.
 * `compareAndAppend` calls are 1-indexed in order: configure (1), start (2), each resolved
 * effect afterwards.
 */
class FaultedJournal implements KernelJournal {
  readonly staged: string[] = []
  private calls = 0

  constructor(
    private readonly inner: KernelJournal,
    private readonly fault: (call: number) => Error | undefined,
  ) {}

  async compareAndAppend(...args: Parameters<KernelJournal["compareAndAppend"]>) {
    const error = this.fault(++this.calls)
    if (error) throw error
    return this.inner.compareAndAppend(...args)
  }

  async stageOutboundEnvelope(operationId: string, envelopeJson: string): Promise<void> {
    this.staged.push(envelopeJson)
    return this.inner.stageOutboundEnvelope(operationId, envelopeJson)
  }
  async clearOutboundEnvelope(operationId: string): Promise<void> {
    return this.inner.clearOutboundEnvelope(operationId)
  }
  async readOutboundEnvelope(operationId: string): Promise<string | undefined> {
    return this.inner.readOutboundEnvelope(operationId)
  }
  async head(...args: Parameters<KernelJournal["head"]>) {
    return this.inner.head(...args)
  }
  async readFrom(...args: Parameters<KernelJournal["readFrom"]>) {
    return this.inner.readFrom(...args)
  }
  async recordsAfter(...args: Parameters<KernelJournal["recordsAfter"]>) {
    return this.inner.recordsAfter(...args)
  }
  async compareAndInstallCheckpoint(...args: Parameters<KernelJournal["compareAndInstallCheckpoint"]>) {
    return this.inner.compareAndInstallCheckpoint(...args)
  }
  async latestCheckpoint(...args: Parameters<KernelJournal["latestCheckpoint"]>) {
    return this.inner.latestCheckpoint(...args)
  }
  async ackCheckpoint(...args: Parameters<KernelJournal["ackCheckpoint"]>) {
    return this.inner.ackCheckpoint(...args)
  }
  async pruneAckedPrefix(...args: Parameters<KernelJournal["pruneAckedPrefix"]>) {
    return this.inner.pruneAckedPrefix(...args)
  }
}

/** A record embeds the envelope that produced it, canonically re-serialized: `canonical_input.data`. */
function envelopeOf(recordBytes: Uint8Array): Record<string, unknown> {
  const record = JSON.parse(Buffer.from(recordBytes).toString("utf8")) as {
    canonical_input?: { data?: string }
  }
  const data = record.canonical_input?.data
  if (typeof data !== "string") throw new Error("record embeds no canonical_input envelope")
  return JSON.parse(Buffer.from(data, "base64").toString("utf8")) as Record<string, unknown>
}

describe("durable restart recovery — FileKernelJournal end to end (0.2.65 S2)", () => {
  let dir: string

  beforeEach(async () => {
    dir = await mkdtemp(join(tmpdir(), "ds-restart-"))
  })

  afterEach(async () => {
    await rm(dir, { recursive: true, force: true })
  })

  it("an uninterrupted run fixes the terminal every restart must reproduce", async () => {
    const log = new FileSessionLog(join(dir, "baseline"))
    const executions = { count: 0 }
    expect(await collectText(runnerFor(log, executions).run({ sessionId: SESSION, goal: "use ping then finish" })))
      .toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
  })

  it("journal-only ladder: a crash after commit before the effect resumes from journal bytes alone", async () => {
    const crashDir = join(dir, "journal-ladder")
    const logBefore = new FileSessionLog(crashDir)
    await logBefore.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    await driveToPendingToolEffect(newRuntime(logBefore.kernelJournal))
    const frozen = await chain(logBefore.kernelJournal)
    expect(frozen.length).toBeGreaterThanOrEqual(3)

    // Process restart: a fresh FileSessionLog reopens the same directory. The frozen prefix
    // must come back byte-identical — the bytes are the whole contract.
    const logAfter = new FileSessionLog(crashDir)
    expect(await chain(logAfter.kernelJournal)).toEqual(frozen)

    const executions = { count: 0 }
    expect(await collectText(runnerFor(logAfter, executions).wake(SESSION))).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
    const resumed = await chain(logAfter.kernelJournal)
    expect(resumed.slice(0, frozen.length)).toEqual(frozen)
    expect(resumed.length).toBeGreaterThan(frozen.length)
  })

  it("checkpoint+tail ladder: a crash after install/ack/reclaim restores through the checkpoint", async () => {
    const crashDir = join(dir, "checkpoint-ladder")
    const logBefore = new FileSessionLog(crashDir)
    await logBefore.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    const kernel = new (getKernel().CanonicalKernel)()
    const runtime = new CanonicalRunnerRuntime(kernel, logBefore.kernelJournal, OP, RUNTIME_OPTIONS)
    await driveToPendingToolEffect(runtime)
    const frozen = await chain(logBefore.kernelJournal)

    // The §12.3 boundary — install, ack, reclaim — runs before the process dies. The pending
    // tool effect lives inside the checkpoint; the covered prefix is reclaimed.
    const installed = await new CanonicalKernelHost(kernel, logBefore.kernelJournal, OP).checkpoint()
    expect(installed.acknowledged).toBe(true)
    const retained = await chain(logBefore.kernelJournal)
    expect(retained.length).toBeLessThan(frozen.length)

    const logAfter = new FileSessionLog(crashDir)
    expect((await logAfter.kernelJournal.latestCheckpoint(OP))?.acknowledged).toBe(true)
    expect(await chain(logAfter.kernelJournal)).toEqual(retained)

    // The wake restore takes the checkpoint+tail ladder: latestCheckpoint + recordsAfter(
    // covered_head). The reclaimed prefix never returns; the run continues past it.
    const executions = { count: 0 }
    expect(await collectText(runnerFor(logAfter, executions).wake(SESSION))).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
    const resumed = await chain(logAfter.kernelJournal)
    expect(resumed.slice(0, retained.length)).toEqual(retained)
    expect(resumed.every(entry => entry.step_seq > installed.through_step_seq)).toBe(true)
  })

  it("a CAS conflict during append rebuilds and retries the byte-identical envelope (crash-point #8)", async () => {
    const crashDir = join(dir, "cas-conflict")
    const log = new FileSessionLog(crashDir)
    await log.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    const faulted = new FaultedJournal(log.kernelJournal, call =>
      call === 3 ? new JournalCasConflictError("another writer took this position") : undefined)
    const runtime = new CanonicalRunnerRuntime(new (getKernel().CanonicalKernel)(), faulted, OP, RUNTIME_OPTIONS)

    // Configure and start land cleanly; the effect resolution hits a stale expected head.
    await runtime.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "ping", description: "Ping", parameters: { type: "object", properties: {} } }],
    })
    const first = await runtime.startAgent(
      { goal: "use ping then finish", criteria: [] },
      { exposure_baseline: ["ping"] },
    )
    expect(first?.kind).toBe("call_provider")
    const before = await chain(log.kernelJournal)

    // The host must absorb the conflict: abort the prepared step, rebuild from the journal,
    // retry the same bytes. The caller sees an ordinary success.
    const pending = await runtime.applyHostEvent({
      kind: "provider_result",
      effect_id: first.effectId,
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call_ping", name: "ping", arguments: {} }],
      },
      stop_reason: "tool_use",
    })
    expect(pending?.kind).toBe("execute_tool")

    // The committed record embeds the envelope staged before the conflict — the retry was
    // the same input, so the record digest is the one the un-conflicted writer determined.
    // (The record re-serializes the envelope canonically; content is the comparison unit.)
    const staged = faulted.staged.at(-1)
    expect(staged).toBeDefined()
    const after = await chain(log.kernelJournal)
    expect(after.length).toBe(before.length + 1)
    expect(envelopeOf((await log.kernelJournal.readFrom(OP)).at(-1)!.record_bytes))
      .toEqual(JSON.parse(staged!))
  })

  it("a crash between staging the outbound envelope and its append drains byte-identically on wake", async () => {
    const crashDir = join(dir, "crash-window")
    const log = new FileSessionLog(crashDir)
    await log.append(SESSION, {
      kind: "run_started",
      run_id: RUN_ID,
      goal: "use ping then finish",
      criteria: [],
    })
    const faulted = new FaultedJournal(log.kernelJournal, call =>
      call === 3 ? new JournalIoError("simulated crash after stage") : undefined)
    const runtime = new CanonicalRunnerRuntime(new (getKernel().CanonicalKernel)(), faulted, OP, RUNTIME_OPTIONS)
    await runtime.applyHostEvent({
      kind: "set_tools",
      tools: [{ name: "ping", description: "Ping", parameters: { type: "object", properties: {} } }],
    })
    const first = await runtime.startAgent(
      { goal: "use ping then finish", criteria: [] },
      { exposure_baseline: ["ping"] },
    )
    expect(first?.kind).toBe("call_provider")

    // The storage layer dies after staging the resolve envelope but before its append-ack:
    // the record never lands, the effect stays pending in the durable state, and the staged
    // bytes survive (append-before failures must not clear them).
    await expect(runtime.applyHostEvent({
      kind: "provider_result",
      effect_id: first.effectId,
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call_ping", name: "ping", arguments: {} }],
      },
      stop_reason: "tool_use",
    })).rejects.toThrow("simulated crash after stage")
    const staged = faulted.staged.at(-1)
    expect(staged).toBeDefined()
    expect((await chain(log.kernelJournal)).length).toBe(2)
    expect(await log.kernelJournal.readOutboundEnvelope(OP)).toBe(staged)

    // Restart: wake rebuilds from the journal, drains the staged envelope byte-identically
    // (the resolved record embeds exactly that input), and the run continues to the same
    // terminal with the effect executed exactly once.
    const logAfter = new FileSessionLog(crashDir)
    expect(await logAfter.kernelJournal.readOutboundEnvelope(OP)).toBe(staged)
    const executions = { count: 0 }
    expect(await collectText(runnerFor(logAfter, executions).wake(SESSION))).toBe(FINAL_TEXT)
    expect(executions.count).toBe(1)
    // The drained step landed at exactly the position the crashed process was claiming;
    // the continuation appends the tool-result and final turns past it.
    const after = await chain(logAfter.kernelJournal)
    expect(after.length).toBeGreaterThanOrEqual(3)
    expect(after[2]!.step_seq).toBe(2)
    expect(envelopeOf((await logAfter.kernelJournal.readFrom(OP))[2]!.record_bytes))
      .toEqual(JSON.parse(staged!))
  })
})
