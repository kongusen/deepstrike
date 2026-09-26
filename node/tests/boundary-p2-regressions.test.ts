/**
 * Node SDK → canonical kernel boundary regressions (P2 audit, 0.2.74).
 *
 * P2-1: several napi methods had no panic guard, every kernel fault was reported as `InvalidArg`,
 *       the runner told a malformed input apart by searching error text for "invalidarg", and the
 *       napi signal conversion read unknown vocabulary as defaults and bad payloads as `null`.
 * P2-2: a provider result / tool batch advanced the host's view (new messages, turn count) before
 *       the kernel committed it, so a refused input left the host ahead of the kernel.
 * P2-3: the journal's one outbound slot per operation relied on callers never overlapping two
 *       transitions; the file journal listed the whole record directory on every append (O(n²) over
 *       a run); and the observations of an envelope drained on restore were thrown away.
 */
import { getKernel } from "../src/kernel.js"
import { mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { governancePolicyPatch } from "../src/governance.js"
import { InMemorySessionLog, RuntimeRunner } from "../src/advanced/public.js"
import { replayMessagesAsync } from "../src/runtime/runner.js"
import type { ModelMessage } from "../src/types.js"
import { FileKernelJournal, InMemoryKernelJournal } from "../src/runtime/kernel-journal.js"
import {
  CanonicalKernelHost,
  CanonicalKernelRejectedError,
  CanonicalRunnerRuntime,
  isInvalidInputError,
} from "../src/runtime/canonical-kernel-step.js"

describe("P2-1: binding faults carry the kernel's own classification", () => {
  it("reports a kernel fault as a fault, not as a malformed argument", () => {
    const { CanonicalKernel } = getKernel()
    let caught: { code?: string; message?: string } | undefined
    try {
      new CanonicalKernel().checkpointCandidate()
    } catch (error) {
      caught = error as { code?: string; message?: string }
    }
    expect(caught?.code).toBe("GenericFailure")
    expect(JSON.parse(caught?.message ?? "{}").code).toBe("invalid_lifecycle")
  })

  it("refuses unknown signal vocabulary and a malformed payload instead of defaulting", () => {
    const { SignalRouter } = getKernel() as unknown as {
      SignalRouter: new (size: number) => { ingest(signal: Record<string, unknown>, lifecycle: string): string }
    }
    const router = new SignalRouter(4)
    const signal = { id: "s1", source: "custom", signalType: "event", urgency: "high", summary: "x", payload: "{}", timestampMs: 1 }
    expect(() => router.ingest({ ...signal, urgency: "critcal" }, "running")).toThrow(/urgency "critcal"/)
    expect(() => router.ingest({ ...signal, source: "webhook" }, "running")).toThrow(/source "webhook"/)
    expect(() => router.ingest({ ...signal, payload: "{bad" }, "running")).toThrow(/payload is not JSON/)
    expect(() => router.ingest({ ...signal, timestampMs: -1 }, "running")).toThrow(/timestampMs/)
    expect(router.ingest(signal, "running")).toBe("interrupt")
  })

  it("classifies a run failure by the fault code, never by message text", () => {
    expect(isInvalidInputError(new CanonicalKernelRejectedError(JSON.stringify({ code: "malformed_envelope", message: "x" })))).toBe(true)
    expect(isInvalidInputError(new CanonicalKernelRejectedError(JSON.stringify({ code: "invalid_lifecycle", message: "x" })))).toBe(false)
    expect(isInvalidInputError(new Error("tool said: invalid argument supplied"))).toBe(false)
    expect(isInvalidInputError(Object.assign(new Error("bad"), { code: "InvalidArg" }))).toBe(true)
  })
})

describe("P2-2: a refused input leaves the host where the kernel is", () => {
  it("does not advance the turn or record messages for a provider result the kernel refused", async () => {
    const { CanonicalKernel } = getKernel()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), new InMemoryKernelJournal(), "op-p2-2", { maxContextTokens: 100_000 })
    const first = await rt.startAgent({ goal: "answer" })
    expect(first?.kind).toBe("call_provider")
    rt.drainNewMessages()
    const turnsBefore = rt.turn()

    await expect(rt.applyHostEvent({
      kind: "provider_result",
      effect_id: "step:999:effect:0",
      message: { role: "assistant", content: "stale answer", toolCalls: [] },
    })).rejects.toThrow()

    expect(rt.turn()).toBe(turnsBefore)
    expect(rt.drainNewMessages()).toEqual([])
  })
})

describe("P2-3: journal transitions are serialized, cheap, and lose no facts", () => {
  it("never lets two transitions of one operation share the outbound slot", async () => {
    const { CanonicalKernel } = getKernel()
    const journal = new InMemoryKernelJournal()
    const events: string[] = []
    const stage = journal.stageOutboundEnvelope.bind(journal)
    const clear = journal.clearOutboundEnvelope.bind(journal)
    journal.stageOutboundEnvelope = async (op, json) => { events.push("stage"); await new Promise(r => setTimeout(r, 5)); return stage(op, json) }
    journal.clearOutboundEnvelope = async op => { events.push("clear"); return clear(op) }
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, "op-serial", { maxContextTokens: 100_000 })
    await rt.startAgent({ goal: "answer" })
    const host = (rt as unknown as { host: CanonicalKernelHost }).host
    events.length = 0
    const update = (progress: string) => host.transition({ kind: "host_control", command: { kind: "update_task", update: { progress } } })
    const outcomes = await Promise.allSettled([update("a"), update("b")])
    expect(outcomes.map(outcome => outcome.status)).toEqual(["fulfilled", "fulfilled"])
    expect(events).toEqual(["stage", "clear", "stage", "clear"])
  })

  it("finds the head of a file journal without listing the directory on every append", async () => {
    const root = mkdtempSync(join(tmpdir(), "p2-3-journal-"))
    try {
      const journal = new FileKernelJournal(root)
      let listings = 0
      const list = (journal as unknown as { recordSeqs(op: string): Promise<number[]> }).recordSeqs.bind(journal)
      ;(journal as unknown as { recordSeqs(op: string): Promise<number[]> }).recordSeqs = async op => { listings += 1; return list(op) }
      let head: string | undefined
      for (let step = 0; step < 40; step += 1) {
        const receipt = await journal.compareAndAppend("op", head, {
          step_seq: step, record_digest: `sha256:${String(step).padStart(64, "0")}`, record_bytes: new Uint8Array([step]),
        })
        head = receipt.record_digest
      }
      expect(listings).toBeLessThanOrEqual(1)
      expect((await journal.head("op"))?.step_seq).toBe(39)
      // another writer advancing the chain is still seen
      const other = new FileKernelJournal(root)
      await other.compareAndAppend("op", head, { step_seq: 40, record_digest: `sha256:${"a".repeat(64)}`, record_bytes: new Uint8Array([1]) })
      expect((await journal.head("op"))?.step_seq).toBe(40)
    } finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  it("keeps the observations of the envelope a restore drains", async () => {
    const { CanonicalKernel } = getKernel()
    const journal = new InMemoryKernelJournal()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, "op-drain", { maxContextTokens: 100_000 })
    await rt.startAgent({ goal: "answer" })
    // A crash after staging and before the append: the envelope is all that survives.
    await journal.stageOutboundEnvelope("op-drain", JSON.stringify({
      operation_id: "op-drain",
      input_id: "crash-window",
      observed_at_ms: String(Date.now() + 1_000),
      input: {
        kind: "host_control",
        command: { kind: "apply_policy_patch", expected_revision: "0", patch: governancePolicyPatch({ vetoes: ["x"] }) },
      },
    }))
    const woken = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, "op-drain", { maxContextTokens: 100_000 })
    await woken.restore()
    expect(woken.drainHostObservations().map(observation => observation.kind)).toContain("live_policy_changed")
  })
})

describe("P2-4: a workflow node's dependents start as soon as it completes", () => {
  it("runs a dependent of the fast node while its slow sibling is still working", async () => {
    let releaseSlow!: () => void
    const slowReleased = new Promise<void>(resolve => { releaseSlow = resolve })
    const started: string[] = []
    const runner = new RuntimeRunner({
      sessionLog: new InMemorySessionLog(),
      maxTokens: 8000,
      subAgentOrchestrator: {
        async run(ctx: { manifest: { agent_id: string }; spec: { goal: string } }) {
          started.push(ctx.spec.goal)
          if (ctx.spec.goal.includes("slow")) {
            // Released only by the dependent of the fast node: behind a round barrier the
            // dependent could not start, and this would time out.
            await Promise.race([slowReleased, new Promise((_, reject) => setTimeout(() => reject(new Error("round barrier")), 2_000))])
          }
          if (ctx.spec.goal.includes("after fast")) releaseSlow()
          const id = ctx.manifest.agent_id
          return {
            agentId: id,
            result: { termination: "completed", finalMessage: { role: "assistant", content: id, toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 1 },
          }
        },
      } as never,
    } as never)

    const outcome = await runner.runWorkflow({
      nodes: [
        { task: "slow worker", role: "explore" },
        { task: "fast worker", role: "explore" },
        { task: "after fast", role: "plan", dependsOn: [1] },
      ],
    }, { sessionId: "p2-4" })

    expect(outcome.nodeOutcomes.map(node => node.status)).toEqual(["completed", "completed", "completed"])
    expect(started.findIndex(goal => goal.includes("after fast"))).toBeGreaterThan(-1)
  })
})

describe("P2-5: an archived tool result survives replay", () => {
  it("keeps the structured parts of messages restored from a page_out archive", async () => {
    const archived: ModelMessage[] = [
      { role: "assistant", content: "", toolCalls: [{ id: "call_a", name: "read", arguments: "{}" }] },
      { role: "tool", content: "", toolCalls: [], contentParts: [{ type: "tool_result", callId: "call_a", output: "file body", isError: false }] },
    ]
    const replayed = await replayMessagesAsync(
      [{ seq: 1, event: { kind: "page_out", turn: 2, archive_ref: "archive:1" } as never }],
      undefined,
      async () => archived,
    )
    const tool = replayed.find(message => message.role === "tool")
    expect(tool?.contentParts).toEqual([{ type: "tool_result", callId: "call_a", output: "file body", isError: false }])
  })
})

describe("P2-6: run-context isolation holds at the kernel boundary", () => {
  it("refuses a receipt addressed from another operation", async () => {
    const { CanonicalKernel } = getKernel()
    const journal = new InMemoryKernelJournal()
    const rt = new CanonicalRunnerRuntime(new CanonicalKernel(), journal, "op-owner", { maxContextTokens: 100_000 })
    const pending = await rt.startAgent({ goal: "answer" })
    if (pending?.kind !== "call_provider") throw new Error(`expected call_provider, got ${pending?.kind}`)
    const owner = (rt as unknown as { host: CanonicalKernelHost }).host
    const foreign = new CanonicalKernelHost(owner.kernel, journal, "op-foreign")
    let refused: unknown
    try {
      await foreign.transition({
        kind: "resolve_effect",
        effect_id: pending.effectId,
        outcome: { status: "failed", failure: { kind: "transport_exhausted", message: "x", retryable: true } },
      })
    } catch (error) {
      refused = error
    }
    expect(refused).toBeInstanceOf(CanonicalKernelRejectedError)
    expect((refused as CanonicalKernelRejectedError).fault.code).toBe("operation_mismatch")
  })
})
