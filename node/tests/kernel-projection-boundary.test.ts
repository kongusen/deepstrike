import {
  messageToKernelMessage,
  taskUpdateToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
} from "../src/runtime/kernel-step.js"

describe("kernel projection adapters", () => {
  it("projects messages and tool schemas with parsed kernel fields", () => {
    expect(messageToKernelMessage({
      role: "assistant",
      content: "done",
      toolCalls: [{ id: "call-1", name: "lookup", arguments: '{"query":"Ada"}' }],
    })).toEqual({
      role: "assistant",
      content: "done",
      tool_calls: [{ id: "call-1", name: "lookup", arguments: { query: "Ada" } }],
    })

    expect(toolSchemaToKernel({
      name: "lookup",
      description: "Look up a record",
      parameters: "invalid-json",
      providerOptions: { openai: { strict: true } },
    })).toEqual({
      name: "lookup",
      description: "Look up a record",
      parameters: {},
    })
  })

  it("projects tool results and task updates into snake-case kernel fields", () => {
    expect(toolResultToKernel({
      callId: "call-1",
      output: "ok",
      isError: false,
      isFatal: true,
      errorKind: "provider_failure",
    })).toEqual({
      call_id: "call-1",
      output: "ok",
      is_error: false,
      is_fatal: true,
      error_kind: "provider_failure",
    })

    expect(taskUpdateToKernel({
      plan: ["inspect", "verify"],
      currentStep: 1,
      progress: "halfway",
      blockedOn: ["approval"],
      preservedRefs: ["run-1"],
    })).toEqual({
      plan: ["inspect", "verify"],
      current_step: 1,
      progress: "halfway",
      scratchpad: undefined,
      blocked_on: ["approval"],
      preserved_refs: ["run-1"],
    })
  })
})
