import type { KernelRuntimeHandle } from "../../src/runtime/kernel-step.js"
import { durableKernelStep } from "../../src/runtime/kernel-step.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"

function plannedStep() {
  return {
    version: 2,
    operation_id: "node-operation-1",
    input_event_id: "node-operation-1-event-1",
    step_seq: 1,
    actions: [{ kind: "call_provider", effect_id: "effect-1", context: {}, tools: [] }],
    observations: [{ kind: "run_started" }],
    faults: [],
  }
}

function fakeRuntime(phases: string[]): KernelRuntimeHandle {
  const step = plannedStep()
  return {
    step: () => JSON.stringify(step),
    prepareStep: inputJson => {
      phases.push("prepare")
      return JSON.stringify({
        status: "prepared",
        base_generation: 0,
        prepare_token: "token-1",
        input: JSON.parse(inputJson),
        step,
      })
    },
    commitPrepared: token => {
      expect(token).toBe("token-1")
      phases.push("commit")
      return JSON.stringify(step)
    },
    abortPrepared: token => {
      expect(token).toBe("token-1")
      phases.push("abort")
    },
    snapshot: () => JSON.stringify({
      snapshot_version: 2,
      abi_version: 2,
      initial_policy: {
        max_tokens: 8_000,
        max_turns: 25,
        max_total_tokens: "0",
      },
      lifecycle: "created",
      next_step_seq: 1,
      snapshot_input_limit: 10_000,
      max_input_bytes: 16_777_216,
      snapshot_journal_bytes_limit: 67_108_864,
      accepted_input_bytes: 0,
      accepted_inputs: [],
    }),
    restore: () => undefined,
    diagnostics: () => "{}",
    isTerminal: () => false,
    turn: () => 0,
    recoveryContentBytes: () => 1_024,
    render: () => ({ systemText: "", systemStable: "", systemKnowledge: "", turns: [] }),
    drainNewMessages: () => [],
    preservedRefs: () => [],
  }
}

describe("durableKernelStep", () => {
  it("publishes the committed step only after genesis and transaction durability", async () => {
    const phases: string[] = []
    class OrderedLog extends InMemorySessionLog {
      override async appendKernelGenesis(...args: Parameters<InMemorySessionLog["appendKernelGenesis"]>) {
        phases.push("genesis")
        return super.appendKernelGenesis(...args)
      }
      override async compareAndAppendKernelTransaction(
        ...args: Parameters<InMemorySessionLog["compareAndAppendKernelTransaction"]>
      ) {
        phases.push("durable_append")
        return super.compareAndAppendKernelTransaction(...args)
      }
    }

    const step = await durableKernelStep(
      fakeRuntime(phases),
      new OrderedLog(),
      "session",
      { kind: "start_run", task: { goal: "test", criteria: [] } },
    )

    expect(step.actions).toHaveLength(1)
    expect(phases).toEqual(["genesis", "prepare", "durable_append", "commit"])
  })

  it("aborts the prepared transition and publishes nothing when durable append fails", async () => {
    const phases: string[] = []
    class FailingLog extends InMemorySessionLog {
      override async compareAndAppendKernelTransaction(): Promise<never> {
        phases.push("durable_append")
        throw new Error("disk unavailable")
      }
    }

    await expect(durableKernelStep(
      fakeRuntime(phases),
      new FailingLog(),
      "session",
      { kind: "start_run", task: { goal: "test", criteria: [] } },
    )).rejects.toThrow("disk unavailable")
    expect(phases).toEqual(["prepare", "durable_append", "abort"])
    expect(phases).not.toContain("commit")
  })
})
