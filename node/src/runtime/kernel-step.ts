import type {
  EntropySample,
  Message,
  RenderedContext,
  TaskUpdate,
  ToolCall,
  ToolResult,
  ToolSchema,
} from "../types.js"
import type { SkillMetadata } from "../skills/loader.js"
import type { RollbackReason } from "./session-log.js"

const CANONICAL_CONTENT_PARTS_PREFIX = "[[deepstrike-content-parts]]"

export function encodeCanonicalContentParts(parts: unknown[]): string {
  return `${CANONICAL_CONTENT_PARTS_PREFIX}${Buffer.from(JSON.stringify(parts)).toString("base64url")}`
}

function decodeCanonicalContentParts(content: string): Array<Record<string, unknown>> | undefined {
  if (!content.startsWith(CANONICAL_CONTENT_PARTS_PREFIX)) return undefined
  try {
    const decoded = JSON.parse(
      Buffer.from(content.slice(CANONICAL_CONTENT_PARTS_PREFIX.length), "base64url").toString("utf8"),
    ) as unknown
    return Array.isArray(decoded)
      ? decoded.filter((part): part is Record<string, unknown> =>
          Boolean(part) && typeof part === "object")
      : undefined
  } catch {
    return undefined
  }
}

export interface PaceDecision {
  action: "continue" | "sleep" | "stop"
  delayMs?: number
  reason: string
  /** Set when the kernel trap coerced the model's proposal (clamped delay / forced stop). */
  coercedFrom?: string
}

export interface KernelLoopResult {
  termination: string
  turnsUsed: number
  totalTokensUsed: number
  finalMessage?: Message
  /** ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs). */
  paceDecision?: PaceDecision
}

export type MilestoneVerifierKind =
  | { kind: "machine_check" }
  | { kind: "harness_eval" }
  | { kind: "llm_judge" }
  | { kind: "human_approval" }
  | { kind: "external_command"; cmd: string }

export type KernelRunnerAction =
  | { kind: "call_provider"; effectId: string; context: RenderedContext; tools: ToolSchema[] }
  | { kind: "execute_tool"; effectId: string; calls: ToolCall[] }
  | {
      kind: "request_approval"
      effectId: string
      requests: Array<{ callId: string; tool: string; arguments: string; reason: string }>
    }
  | {
      kind: "spawn_workflow"
      effectId: string
      nodes: Array<Record<string, unknown>>
      budget?: Record<string, unknown>
    }
  | {
      kind: "preempt_sub_agents"
      effectId: string
      agentIds: string[]
      attempts?: Array<{ task_id: string; attempt_id: string }>
      reason: string
    }
  | { kind: "persist_memory"; effectId: string; memory: Record<string, unknown> }
  | { kind: "query_memory"; effectId: string; query: Record<string, unknown>; requestedK: number }
  | {
      kind: "archive_page_out"
      effectId: string
      turn?: number
      action?: string
      summary?: string
      archived?: Message[]
      tier?: string
      handleId?: string
      payload?: {
        content: string
        digest: string
        original_size: string
        preview?: string
      }
    }
  | { kind: "load_payload"; effectId: string; handleId: string; payloadRef: string }
  | {
      kind: "evaluate_milestone"
      effectId: string
      phaseId: string
      criteria: string[]
      verifier?: MilestoneVerifierKind
      requiredEvidence: string[]
    }
  | { kind: "unsupported_effect"; effectId: string; effectKind: string }
  | { kind: "done"; effectId: string; result: KernelLoopResult }

export interface KernelObservation {
  kind: string
  /** control_request_rejected: stable control-plane operation name and optional subject id. */
  operation?: string
  subject?: string
  action?: string
  rho_after?: number
  sprint?: number
  summary?: string
  archived_count?: number
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
  parent_task_id?: string
  role?: string
  isolation?: string
  context_inheritance?: string
  permitted_capability_ids?: string[]
  // tool_gated (governance AskUser): the call needing user approval.
  call_id?: string
  tool?: string
  // signal_delivery_disposed: the correlated routing decision.
  operation_id?: string
  delivery_id?: string
  attempt?: number
  signal_id?: string
  disposition?: string
  queue_depth?: number
  // Phase 2: budget_exceeded observation — which budget axis fired.
  budget?: string
  reservation_id?: string
  tokens?: number
  subagents?: number
  rounds?: number
  // Phase 2: suspended observation — loop suspended awaiting external resolution.
  pending_calls?: string[]
  pending_call_ids?: string[]
  // Phase 2: resumed observation — loop resumed with approved/denied calls.
  approved?: string[]
  denied?: string[]
  tier?: string
  message_count?: number
  archive_ref?: string
  original_size?: number
  preview_size?: number
  // Phase 7 / M3: Memory observations
  record_id?: string
  scope?: { tenant_id: string; namespace: string }
  name?: string
  memory_kind?: string
  size_bytes?: number
  query?: string
  requested_k?: number
  requires_async_response?: boolean
  // M3 memory_recalled: journaled recall lifecycle mirrored into the durable store.
  recalls?: Array<{ record_id: string; recall_count: number; last_recalled_at: number }>
  // M4 promotion_suggested: a recalled record crossed the promotion threshold (advisory).
  recall_count?: number
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
  node_outcomes?: import("../types/agent.js").KernelWorkflowNodeOutcome[]
  /** nodes_rejected. */
  node_index?: number
  // entropy_sample / entropy_alert: kernel session-entropy measurement + opt-in watch trip.
  score?: number
  rho?: number
  repeat_pressure?: number
  failure_rate?: number
  rollbacks_in_window?: number
  window_turns?: number
  threshold?: number
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
  // SPC-015-01: structured grants are caller-supplied rather than inferred from scalar frontmatter.
  if (skill.capabilityGrants?.length) out.capability_grants = skill.capabilityGrants
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

/** Camel-case an `entropy_sample` kernel observation into the SDK's `EntropySample`. */
export function entropySampleFromObservation(obs: KernelObservation): EntropySample {
  return {
    turn: obs.turn ?? 0,
    score: obs.score ?? 0,
    rho: obs.rho ?? 0,
    repeatPressure: obs.repeat_pressure ?? 0,
    failureRate: obs.failure_rate ?? 0,
    rollbacksInWindow: obs.rollbacks_in_window ?? 0,
    windowTurns: obs.window_turns ?? 0,
  }
}

export function kernelMessageToSdk(raw: Record<string, unknown>): Message {
  const content = raw.content
  const canonicalParts = typeof content === "string"
    ? decodeCanonicalContentParts(content)
    : undefined
  const structuredContent = canonicalParts ?? (Array.isArray(content) ? content : undefined)
  const message: Message = {
    role: raw.role as Message["role"],
    content: canonicalParts
      ? canonicalParts
          .filter(part => part.type === "text")
          .map(part => String(part.text ?? ""))
          .join("")
      : typeof content === "string"
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
      id: String(tc.call_id ?? tc.id ?? ""),
      name: String(tc.name ?? ""),
      arguments: JSON.stringify(tc.arguments ?? {}),
    })),
  }
  if (typeof (raw.tokens ?? raw.token_count) === "number") {
    message.tokenCount = Number(raw.tokens ?? raw.token_count)
  }
  if (structuredContent) {
    message.contentParts = structuredContent
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
  } else if (typeof raw.tool_call_id === "string") {
    message.contentParts = [{
      type: "tool_result",
      callId: raw.tool_call_id,
      output: message.content,
      isError: false,
    }]
  }
  return message
}

export function renderedContextToSdk(raw: Record<string, unknown>): RenderedContext {
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
