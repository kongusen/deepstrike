import type {
  Message,
  RenderedContext,
  TaskUpdate,
  ToolCall,
  ToolResult,
  ToolSchema,
} from "../types.js"
import type { SkillMetadata } from "../skills/loader.js"
import type { RollbackReason } from "./session-log.js"

export const KERNEL_ABI_VERSION = 1

export interface KernelRuntimeHandle {
  step(inputJson: string): string
  isTerminal(): boolean
  turn(): number
  recoveryContentBytes(): number
  render(): RenderedContext
  drainNewMessages(): Message[]
  preservedRefs(): string[]
}

export interface KernelLoopResult {
  termination: string
  turnsUsed: number
  totalTokensUsed: number
}

export type MilestoneVerifierKind =
  | { kind: "machine_check" }
  | { kind: "harness_eval" }
  | { kind: "llm_judge" }
  | { kind: "human_approval" }
  | { kind: "external_command"; cmd: string }

export type KernelRunnerAction =
  | { kind: "call_provider"; context: RenderedContext; tools: ToolSchema[] }
  | { kind: "execute_tool"; calls: ToolCall[] }
  | {
      kind: "evaluate_milestone"
      phaseId: string
      criteria: string[]
      verifier?: MilestoneVerifierKind
      requiredEvidence: string[]
    }
  | { kind: "done"; result: KernelLoopResult }

export interface KernelObservation {
  kind: string
  action?: string
  rho_after?: number
  sprint?: number
  summary?: string
  archived?: Message[]
  turn?: number
  checkpoint_history_len?: number
  history_len?: number
  added?: string[]
  removed?: string[]
  change_kind?: string
  capability_id?: string
  version?: string
  mounted_by?: string
  mount_reason?: string
  phase_id?: string
  capabilities_unlocked?: string[]
  evidence?: string[]
  // K1 `knowledge_swept`: keyed entries dropped by a boundary sweep of the knowledge partition.
  removed_keys?: string[]
  tokens_freed?: number
  // `reason` is a RollbackReason for rollback observations, but a plain string
  // for `tool_gated` (governance AskUser) — consumers narrow by `kind`.
  reason?: RollbackReason | string
  agent_id?: string
  parent_session_id?: string
  role?: string
  isolation?: string
  context_inheritance?: string
  permitted_capability_ids?: string[]
  // tool_gated (governance AskUser): the call needing user approval.
  call_id?: string
  tool?: string
  // signal_disposed (in-kernel attention policy): the routing decision.
  signal_id?: string
  disposition?: string
  queue_depth?: number
  // Phase 2: budget_exceeded observation — which budget axis fired.
  budget?: string
  // Phase 2: suspended observation — loop suspended awaiting external resolution.
  pending_calls?: string[]
  // Phase 2: resumed observation — loop resumed with approved/denied calls.
  approved?: string[]
  denied?: string[]
  tier_hint?: string
  // Phase 7 / M3: Memory observations
  memory_id?: string
  memory_kind?: string
  size_bytes?: number
  query_context?: string
  requested_k?: number
  requires_async_response?: boolean
  /** memory_validation_failed (Phase 7). */
  error?: string
  // W0-ABI: workflow lifecycle observations.
  /** workflow_batch_spawned: per-node spawn descriptors (agent_id + goal + role/isolation). */
  nodes?: Array<{
    agent_id: string
    goal: string
    role: string
    isolation: string
    context_inheritance: string
    model_hint?: string
    trust?: string
  }>
  /** workflow_completed. */
  completed?: string[]
  failed?: string[]
}

interface KernelStepJson {
  version: number
  actions: Array<Record<string, unknown>>
  observations: KernelObservation[]
}

function tryParseJson(s: string): unknown {
  try {
    return JSON.parse(s)
  } catch {
    return null
  }
}

export function toolSchemaToKernel(schema: ToolSchema): Record<string, unknown> {
  return {
    name: schema.name,
    description: schema.description,
    parameters: tryParseJson(schema.parameters) ?? {},
  }
}

export function skillMetadataToKernel(skill: SkillMetadata): Record<string, unknown> {
  const out: Record<string, unknown> = {
    name: skill.name,
    description: skill.description,
    estimated_tokens: skill.estimatedTokens ?? 0,
  }
  if (skill.whenToUse) out.when_to_use = skill.whenToUse
  if (skill.effort !== undefined) out.effort = skill.effort
  // P1-B: forward declared tool ids (additive; omitted when empty so existing skills' wire is unchanged).
  if (skill.allowedTools?.length) out.allowed_tools = skill.allowedTools
  return out
}

export function messageToKernelMessage(message: Message): Record<string, unknown> {
  const out: Record<string, unknown> = {
    role: message.role,
    tool_calls: (message.toolCalls ?? []).map(tc => ({
      id: tc.id,
      name: tc.name,
      arguments: tryParseJson(tc.arguments) ?? {},
    })),
  }
  if (message.tokenCount !== undefined) {
    out.token_count = message.tokenCount
  }
  if (message.contentParts && message.contentParts.length > 0) {
    out.content = message.contentParts.map(part => {
      if (part.type === "text") return { type: "text", text: part.text }
      if (part.type === "tool_result") {
        return {
          type: "tool_result",
          call_id: part.callId,
          output: part.output,
          is_error: part.isError,
        }
      }
      if (part.type === "image") {
        return {
          type: "image",
          url: part.url,
          data: part.data,
          media_type: part.mediaType,
          detail: part.detail,
        }
      }
      if (part.type === "audio") {
        return { type: "audio", data: part.data, media_type: part.mediaType }
      }
      return { type: "text", text: message.content }
    })
  } else {
    out.content = message.content
  }
  return out
}

export function toolResultToKernel(result: ToolResult): Record<string, unknown> {
  const out: Record<string, unknown> = {
    call_id: result.callId,
    output: result.output,
    is_error: result.isError,
    is_fatal: result.isFatal ?? false,
    token_count: result.tokenCount ?? null,
  }
  if (result.errorKind !== undefined) {
    out.error_kind = result.errorKind
  }
  return out
}

export function taskUpdateToKernel(update: TaskUpdate): Record<string, unknown> {
  return {
    plan: update.plan,
    current_step: update.currentStep,
    progress: update.progress,
    scratchpad: update.scratchpad,
    blocked_on: update.blockedOn,
    preserved_refs: update.preservedRefs,
  }
}

export function capabilityTool(schema: ToolSchema): Record<string, unknown> {
  return {
    id: schema.name,
    kind: "tool",
    description: schema.description,
    tool_schema: toolSchemaToKernel(schema),
  }
}

export function capabilitySkill(skill: SkillMetadata): Record<string, unknown> {
  return {
    id: skill.name,
    kind: "skill",
    description: skill.description,
    skill: skillMetadataToKernel(skill),
  }
}

export function capabilityMarker(kind: string, id: string, description: string): Record<string, unknown> {
  return { id, kind, description }
}

export function capabilityCommandMount(
  capability: Record<string, unknown>,
  mountedBy = "sdk:runtime",
  mountReason = "dynamic_register",
): Record<string, unknown> {
  return {
    kind: "capability_command",
    command: {
      action: "mount",
      capability,
      mounted_by: mountedBy,
      mount_reason: mountReason,
    },
  }
}

export function capabilityCommandUnmount(capabilityKind: string, id: string): Record<string, unknown> {
  return {
    kind: "capability_command",
    command: { action: "unmount", kind: capabilityKind, id },
  }
}

function parseStep(raw: string): KernelStepJson {
  return JSON.parse(raw) as KernelStepJson
}

function kernelMessageToSdk(raw: Record<string, unknown>): Message {
  const content = raw.content
  const message: Message = {
    role: raw.role as Message["role"],
    content: typeof content === "string"
      ? content
      : Array.isArray(content)
        ? content
            .filter((part): part is Record<string, unknown> => {
              return typeof part === "object" && part !== null && part.type === "text"
            })
            .map(part => String(part.text ?? ""))
            .join("")
        : "",
    toolCalls: ((raw.tool_calls as Array<Record<string, unknown>>) ?? []).map(tc => ({
      id: String(tc.id ?? ""),
      name: String(tc.name ?? ""),
      arguments: JSON.stringify(tc.arguments ?? {}),
    })),
  }
  if (typeof raw.token_count === "number") {
    message.tokenCount = raw.token_count
  }
  if (Array.isArray(content)) {
    message.contentParts = content
      .filter((part): part is Record<string, unknown> => typeof part === "object" && part !== null)
      .map(part => {
        if (part.type === "text") {
          return { type: "text", text: String(part.text ?? "") }
        }
        if (part.type === "tool_result") {
          return {
            type: "tool_result",
            callId: String(part.call_id ?? ""),
            output: String(part.output ?? ""),
            isError: Boolean(part.is_error),
          }
        }
        if (part.type === "image") {
          return {
            type: "image",
            url: part.url as string | undefined,
            data: part.data as string | undefined,
            mediaType: part.media_type as string | undefined,
            detail: part.detail as "auto" | "low" | "high" | undefined,
          }
        }
        if (part.type === "audio") {
          return {
            type: "audio",
            data: String(part.data ?? ""),
            mediaType: String(part.media_type ?? "audio/wav"),
          }
        }
        return { type: "text", text: "" }
      })
  }
  return message
}

function renderedContextToSdk(raw: Record<string, unknown>): RenderedContext {
  const rawStateTurn = (raw.state_turn ?? raw.stateTurn) as Record<string, unknown> | undefined
  const frozenLen = (raw.frozen_prefix_len ?? raw.frozenPrefixLen) as number | undefined
  const ctx: RenderedContext = {
    systemText: String(raw.system_text ?? raw.systemText ?? ""),
    systemStable: String(raw.system_stable ?? raw.systemStable ?? ""),
    systemKnowledge: String(raw.system_knowledge ?? raw.systemKnowledge ?? ""),
    turns: ((raw.turns as Array<Record<string, unknown>>) ?? []).map(kernelMessageToSdk),
  }
  if (rawStateTurn) ctx.stateTurn = kernelMessageToSdk(rawStateTurn)
  if (typeof frozenLen === "number") ctx.frozenPrefixLen = frozenLen
  return ctx
}

function mapKernelAction(raw: Record<string, unknown>): KernelRunnerAction {
  switch (raw.kind) {
    case "call_provider":
      return {
        kind: "call_provider",
        context: renderedContextToSdk((raw.context as Record<string, unknown>) ?? {}),
        tools: ((raw.tools as Array<Record<string, unknown>>) ?? []).map(t => ({
          name: String(t.name ?? ""),
          description: String(t.description ?? ""),
          parameters: JSON.stringify(t.parameters ?? {}),
        })),
      }
    case "execute_tool":
      return {
        kind: "execute_tool",
        calls: ((raw.calls as Array<Record<string, unknown>>) ?? []).map(c => ({
          id: String(c.id ?? ""),
          name: String(c.name ?? ""),
          arguments: JSON.stringify(c.arguments ?? {}),
        })),
      }
    case "evaluate_milestone":
      return {
        kind: "evaluate_milestone",
        phaseId: String(raw.phase_id ?? ""),
        criteria: (raw.criteria as string[]) ?? [],
        verifier: raw.verifier as MilestoneVerifierKind | undefined,
        requiredEvidence: (raw.required_evidence as string[]) ?? [],
      }
    case "done": {
      const result = (raw.result as Record<string, unknown>) ?? {}
      return {
        kind: "done",
        result: {
          termination: String(result.termination ?? "error"),
          turnsUsed: Number(result.turns_used ?? 0),
          totalTokensUsed: Number(result.total_tokens_used ?? 0),
        },
      }
    }
    default:
      throw new Error(`unknown KernelAction kind: ${String(raw.kind)}`)
  }
}

function stepInput(event: Record<string, unknown>): string {
  return JSON.stringify({ version: KERNEL_ABI_VERSION, event })
}

export function kernelApply(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelObservation[] {
  const step = parseStep(runtime.step(stepInput(event)))
  pending.push(...step.observations)
  return step.observations
}

export function kernelAction(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelRunnerAction {
  const action = kernelMaybeAction(runtime, pending, event)
  if (!action) throw new Error("kernel transition must return one action")
  return action
}

/**
 * Like {@link kernelAction} but tolerates a zero-action step. Used for events
 * whose outcome may not drive a provider call — e.g. a signal the kernel queues
 * or ignores returns no action. Returns `null` in that case.
 */
export function kernelMaybeAction(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
  event: Record<string, unknown>,
): KernelRunnerAction | null {
  const step = parseStep(runtime.step(stepInput(event)))
  pending.push(...step.observations)
  const raw = step.actions[0]
  return raw ? mapKernelAction(raw) : null
}

export function forceCompact(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
): boolean {
  return kernelApply(runtime, pending, { kind: "force_compact" }).some(o => o.kind === "compressed")
}
