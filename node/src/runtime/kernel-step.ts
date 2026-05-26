import type {
  Message,
  RenderedContext,
  TaskUpdate,
  ToolCall,
  ToolResult,
  ToolSchema,
} from "../types.js"
import type { SkillMetadata } from "../skills/loader.js"

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

export type KernelRunnerAction =
  | { kind: "call_provider"; context: RenderedContext; tools: ToolSchema[] }
  | { kind: "execute_tool"; calls: ToolCall[] }
  | { kind: "evaluate_milestone"; phaseId: string; criteria: string[] }
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
  added?: string[]
  removed?: string[]
  phase_id?: string
  capabilities_unlocked?: string[]
  reason?: string
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
  return {
    call_id: result.callId,
    output: result.output,
    is_error: result.isError,
    is_fatal: false,
    token_count: result.tokenCount ?? null,
  }
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
  return {
    systemText: String(raw.system_text ?? raw.systemText ?? ""),
    turns: ((raw.turns as Array<Record<string, unknown>>) ?? []).map(kernelMessageToSdk),
  }
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
  const step = parseStep(runtime.step(stepInput(event)))
  pending.push(...step.observations)
  const raw = step.actions[0]
  if (!raw) throw new Error("kernel transition must return one action")
  return mapKernelAction(raw)
}

export function forceCompact(
  runtime: KernelRuntimeHandle,
  pending: KernelObservation[],
): boolean {
  return kernelApply(runtime, pending, { kind: "force_compact" }).some(o => o.kind === "compressed")
}
