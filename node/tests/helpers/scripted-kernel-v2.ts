import type { KernelRuntimeHandle, KernelStepJson } from "../../src/runtime/kernel-step.js"

type ScriptReply = {
  actions?: Array<Record<string, unknown>>
  observations?: Array<Record<string, unknown>>
  faults?: Array<Record<string, unknown>>
}

type Prepared = {
  token: string
  input: Record<string, unknown>
  step: KernelStepJson
  baseGeneration: number
}

/**
 * Transactional test double for host-driver tests whose scripted behavior is easier to express
 * than a native workflow. It intentionally implements the complete ABI-v2 durability contract;
 * old step-only doubles are invalid because they bypass action publication ordering.
 */
export function scriptedKernelV2(
  handle: (event: Record<string, unknown>) => ScriptReply,
): KernelRuntimeHandle {
  let generation = 0
  let nextStepSeq = 1
  let prepared: Prepared | undefined

  const prepare = (inputJson: string): Prepared => {
    if (prepared) throw new Error("scripted kernel already has a prepared transition")
    const input = JSON.parse(inputJson) as Record<string, unknown>
    const event = input.event as Record<string, unknown>
    const reply = handle(event)
    const operationId = String(input.operation_id)
    const eventId = String(input.event_id)
    const step = {
      version: 2,
      operation_id: operationId,
      input_event_id: eventId,
      step_seq: nextStepSeq,
      actions: reply.actions ?? [],
      observations: reply.observations ?? [],
      faults: reply.faults ?? [],
    } as KernelStepJson
    prepared = {
      token: `scripted-prepare-${nextStepSeq}`,
      input,
      step,
      baseGeneration: generation,
    }
    return prepared
  }

  const commit = (token: string): string => {
    if (!prepared || prepared.token !== token) throw new Error("invalid scripted prepare token")
    const step = prepared.step
    prepared = undefined
    generation += 1
    nextStepSeq += 1
    return JSON.stringify(step)
  }

  return {
    step(inputJson) {
      const staged = prepare(inputJson)
      return commit(staged.token)
    },
    prepareStep(inputJson) {
      const staged = prepare(inputJson)
      return JSON.stringify({
        status: "prepared",
        base_generation: staged.baseGeneration,
        prepare_token: staged.token,
        input: staged.input,
        step: staged.step,
      })
    },
    commitPrepared: commit,
    abortPrepared(token) {
      if (!prepared || prepared.token !== token) throw new Error("invalid scripted prepare token")
      prepared = undefined
    },
    snapshot() {
      if (prepared) throw new Error("cannot snapshot a prepared scripted transition")
      return JSON.stringify({
        snapshot_version: 2,
        abi_version: 2,
        initial_policy: { max_tokens: 8_000, max_turns: 25, max_total_tokens: "0" },
        lifecycle: "running",
        next_step_seq: nextStepSeq,
        snapshot_input_limit: 10_000,
        max_input_bytes: 16_777_216,
        snapshot_journal_bytes_limit: 67_108_864,
        accepted_input_bytes: 0,
        accepted_inputs: [],
      })
    },
    restore() { throw new Error("scripted kernel restore is not supported") },
    diagnostics: () => JSON.stringify({ next_step_seq: nextStepSeq }),
    isTerminal: () => false,
    turn: () => 0,
    recoveryContentBytes: () => 0,
    render: () => ({ systemText: "", systemStable: "", systemKnowledge: "", turns: [] }),
    drainNewMessages: () => [],
    preservedRefs: () => [],
  }
}
