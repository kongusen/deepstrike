import type {
  ModelMessage,
  RenderedContext,
  ToolCall,
  ToolExecutionResult,
  ToolSchema,
  MediaSource,
} from "../types.js"
import type { RollbackReason } from "./session-log.js"

interface TaskUpdate {
  plan?: string[]
  currentStep?: number
  progress?: string
  scratchpad?: string
  blockedOn?: string[]
  preservedRefs?: string[]
}

interface SkillMetadata {
  name: string
  description: string
  whenToUse?: string
  effort?: number
  estimatedTokens?: number
  /** P1-B tool gating: tool ids this skill needs; when active the kernel narrows the toolset to
   *  `stable-core ∪ allowedTools`. Absent ⇒ no narrowing. */
  allowedTools?: string[]
}

export const CANONICAL_CONTENT_PARTS_PREFIX = "[[deepstrike-content-parts]]"

/**
 * Browser/worker-safe transport for typed content. `btoa` and `atob` operate on
 * binary strings, so explicitly bridge UTF-8 bytes rather than assuming Latin-1.
 *
 * Byte contract (content-parts-v1, F14/B5): compact JSON with literal UTF-8, insertion key
 * order, **base64url alphabet, no padding** — identical bytes to the Node and Python SDKs.
 * Decode stays lenient (both alphabets, optional padding) so payloads written before the
 * url-alphabet alignment remain readable.
 */
export function encodeCanonicalContentParts(parts: unknown[]): string {
  const bytes = new TextEncoder().encode(JSON.stringify(parts))
  let binary = ""
  for (const byte of bytes) binary += String.fromCharCode(byte)
  const payload = btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
  return `${CANONICAL_CONTENT_PARTS_PREFIX}${payload}`
}

export function decodeCanonicalContentParts(content: string): Array<Record<string, unknown>> | undefined {
  if (!content.startsWith(CANONICAL_CONTENT_PARTS_PREFIX)) return undefined
  try {
    const raw = content.slice(CANONICAL_CONTENT_PARTS_PREFIX.length)
    const standard = raw.replaceAll("-", "+").replaceAll("_", "/")
    const padded = standard + "=".repeat((4 - (standard.length % 4)) % 4)
    const binary = atob(padded)
    const bytes = Uint8Array.from(binary, character => character.charCodeAt(0))
    const decoded = JSON.parse(new TextDecoder().decode(bytes)) as unknown
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
  finalMessage?: ModelMessage
  /** ③ loop-agent: the kernel-adjudicated after-round decision (absent on non-loop runs). */
  paceDecision?: PaceDecision
}

export type KernelRunnerAction =
  | { kind: "call_provider"; effectId: string; context: RenderedContext; tools: ToolSchema[]; contextEffect: Record<string, unknown> }
  | { kind: "execute_tool"; effectId: string; calls: ToolCall[] }
  | { kind: "request_approval"; effectId: string; requests: Array<{ callId: string; tool: string; arguments: string; reason: string }> }
  | { kind: "spawn_workflow"; effectId: string; nodes: Array<Record<string, unknown>>; budget?: Record<string, unknown> }
  | { kind: "preempt_sub_agents"; effectId: string; agentIds: string[]; attempts?: Array<{ task_id: string; attempt_id: string }>; reason: string }
  | { kind: "persist_memory"; effectId: string; memory: Record<string, unknown> }
  | { kind: "query_memory"; effectId: string; query: Record<string, unknown>; requestedK: number }
  | {
      kind: "archive_page_out"
      effectId: string
      archived: ModelMessage[]
      handleId?: string
      payload?: {
        content: string
        digest: string
        original_size: string
        preview?: string
      }
    }
  | { kind: "load_payload"; effectId: string; handleId: string; payloadRef: string }
  | { kind: "evaluate_milestone"; effectId: string; phaseId: string; criteria: string[]; requiredEvidence?: string[] }
  | { kind: "unsupported_effect"; effectId: string; effectKind: string }
  | { kind: "done"; effectId: string; result: KernelLoopResult }

export interface KernelObservation {
  kind: string
  operation?: string
  subject?: string
  action?: string
  rho_after?: number
  sprint?: number
  summary?: string
  archived_count?: number
  turn?: number
  checkpoint_history_len?: number
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
  reason?: RollbackReason | string
  agent_id?: string
  parent_task_id?: string
  role?: string
  isolation?: string
  context_inheritance?: string
  permitted_capability_ids?: string[]
  history_len?: number
  tier_hint?: string
  call_id?: string
  tool?: string
  operation_id?: string
  delivery_id?: string
  attempt?: number
  signal_id?: string
  disposition?: string
  queue_depth?: number
  budget?: string
  reservation_id?: string
  tokens?: number
  subagents?: number
  rounds?: number
  pending_calls?: string[]
  pending_call_ids?: string[]
  approved?: string[]
  denied?: string[]
  original_size?: number
  preview_size?: number
  tier?: string
  message_count?: number
  archive_ref?: string
  // Phase 7 / M3: Memory observations
  record_id?: string
  scope?: { tenant_id: string; namespace: string }
  name?: string
  memory_kind?: string
  size_bytes?: number
  query?: string
  requested_k?: number
  requires_async_response?: boolean
  // M3 memory_recalled / M4 promotion_suggested.
  recalls?: Array<{ record_id: string; recall_count: number; last_recalled_at: number }>
  recall_count?: number
  /** memory_validation_failed (Phase 7). */
  error?: string
  // W0-ABI: workflow lifecycle observations.
  nodes?: Array<{
    agent_id: string
    goal: string
    role: string
    isolation: string
    context_inheritance: string
    model_hint?: string
  }>
  node_outcomes?: import("./types/agent.js").KernelWorkflowNodeOutcome[]
  node_index?: number
  // entropy_sample / entropy_alert: kernel session-entropy measurement + opt-in watch trip.
  score?: number
  rho?: number
  repeat_pressure?: number
  failure_rate?: number
  rollbacks_in_window?: number
  window_turns?: number
  threshold?: number
  // step_published_effects: the effect manifest of a committed step (host-side audit fact; the
  // kernel never emits it — see the §7.11 note in the spec).
  effects?: Array<{ effect_id: string; kind: string }>
}

/** Presentation/policy facts for archive actions are sourced from committed observations. */
export function archivePresentationFromObservations(
  observations: readonly KernelObservation[],
): { action?: string; summary?: string; tier?: "semantic" | "durable" } {
  const compressed = [...observations].reverse().find(observation => observation.kind === "compressed")
  const action = compressed?.action
  if (action !== "snip_compact" && action !== "micro_compact" && action !== "context_collapse" && action !== "auto_compact") return {}
  return {
    action,
    ...(compressed?.summary ? { summary: compressed.summary } : {}),
    tier: action === "context_collapse" || action === "auto_compact" ? "semantic" : "durable",
  }
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

export function messageToKernelMessage(message: ModelMessage): Record<string, unknown> {
  const out: Record<string, unknown> = {
    role: message.role,
    tool_calls: (message.toolCalls ?? []).map(tc => ({
      id: tc.id,
      name: tc.name,
      arguments: tryParseJson(tc.arguments) ?? {},
    })),
  }
  // Multimodal: serialize typed content parts to the kernel `Content::Parts` shape when present
  // (image/audio must survive the reconstruction→preload path, not just live ingress).
  if (message.contentParts && message.contentParts.length > 0) {
    out.content = message.contentParts.map(part => {
      if (part.type === "text") return { type: "text", text: part.text }
      if (part.type === "tool_result") {
        return { type: "tool_result", call_id: part.callId, output: part.output, is_error: part.isError }
      }
      if (part.type === "image") {
        return { type: "image", source: part.source, media_type: part.mediaType, detail: part.detail }
      }
      if (part.type === "audio") {
        return { type: "audio", source: part.source, media_type: part.mediaType }
      }
      return { type: "text", text: message.content }
    })
  } else {
    out.content = message.content
  }
  return out
}

export function toolResultToKernel(result: ToolExecutionResult): Record<string, unknown> {
  // Usage evidence enters through the host event contract; content alone cannot establish usage.
  const out: Record<string, unknown> = {
    call_id: result.callId,
    output: result.output,
    is_error: result.isError,
    is_fatal: result.isFatal ?? false,
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

export function kernelMessageToSdk(raw: Record<string, unknown>): ModelMessage {
  const content = raw.content
  const message: ModelMessage = {
    role: raw.role as ModelMessage["role"],
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
  if (typeof content === "string") {
    const parts = decodeCanonicalContentParts(content)
    if (parts) {
      const contentParts: NonNullable<ModelMessage["contentParts"]> = []
      for (const part of parts) {
        switch (part.type) {
          case "text":
            contentParts.push({ type: "text", text: String(part.text ?? "") })
            break
          case "tool_result":
            contentParts.push({
            type: "tool_result" as const,
            callId: String(part.call_id ?? ""),
            output: String(part.output ?? ""),
            isError: Boolean(part.is_error),
            })
            break
          case "image":
            contentParts.push({
            type: "image" as const,
            ...(part.source ? { source: part.source as MediaSource } : {}),
            ...(part.media_type ? { mediaType: String(part.media_type) } : {}),
            ...(part.detail === "auto" || part.detail === "low" || part.detail === "high"
              ? { detail: part.detail }
              : {}),
            })
            break
          case "audio":
            contentParts.push({
            type: "audio" as const,
            source: part.source as MediaSource | undefined,
            ...(part.media_type ? { mediaType: String(part.media_type) } : {}),
            })
            break
        }
      }
      message.contentParts = contentParts
    }
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
