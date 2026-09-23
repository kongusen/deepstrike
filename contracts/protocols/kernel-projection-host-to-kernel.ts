/** Core host-to-kernel projections used by the live runner syscall path. */

import type { BoundaryProtocol } from "./types.js"

export const KERNEL_PROJECTION_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "kernel-projection.host-to-kernel",
  family: "host-to-kernel",
  direction: "project",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "runtime/kernel-step:messageToKernelMessage",
      source: { type: "ModelMessage", layer: "host", authority: "host-runtime" },
      target: { type: "KernelMessage", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["role", "content"],
        drops: ["contentParts", "toolCalls"],
        derived: ["tool_calls"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:toolSchemaToKernel",
      source: { type: "ToolSchema", layer: "host", authority: "host-runtime" },
      target: { type: "KernelToolSchema", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["name", "description"],
        drops: ["parameters", "providerOptions"],
        derived: ["parameters"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:toolResultToKernel",
      source: { type: "ToolExecutionResult", layer: "host", authority: "host-runtime" },
      target: { type: "KernelToolResult", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["callId", "output", "isError", "isFatal", "errorKind", "contentParts"],
        derived: ["call_id", "output", "is_error", "is_fatal", "error_kind"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:taskUpdateToKernel",
      source: { type: "TaskUpdate", layer: "host", authority: "host-runtime" },
      target: { type: "KernelTaskUpdate", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["plan", "currentStep", "progress", "scratchpad", "blockedOn", "preservedRefs"],
        derived: ["plan", "current_step", "progress", "scratchpad", "blocked_on", "preserved_refs"],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Kernel projection tests cover snake-case fields, parsed arguments, and optional data handling." },
}
