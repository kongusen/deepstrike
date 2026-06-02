import type { KernelObservation } from "./kernel-step.js"
import type { RollbackReason, SessionEvent } from "./session-log.js"

/** Agent OS kernel event category (Phase 5). */
export type KernelEventCategory = "syscall" | "sched" | "mm" | "proc" | "ipc"

export function categoryForKind(kind: string): KernelEventCategory {
  switch (kind) {
    case "tool_gated":
    case "capability_changed":
      return "syscall"
    case "compressed":
    case "page_out":
    case "page_in":
    case "page_in_requested":
    case "renewed":
    case "context_renewed":
      return "mm"
    case "agent_process_changed":
      return "proc"
    case "signal_disposed":
      return "ipc"
    default:
      return "sched"
  }
}

export function withCategory<T extends { kind: string }>(
  event: T,
): T & { category: KernelEventCategory } {
  return { ...event, category: categoryForKind(event.kind) }
}

type CompressionAction = Extract<SessionEvent, { kind: "compressed" }>["action"]

export function kernelObservationToSessionEvent(
  obs: KernelObservation,
  turn: number,
  opts: {
    nextArchiveStart?: number
    latestSeq?: number
    archiveRef?: string
    preservedRefs?: string[]
    compressionAction?: (action?: string) => CompressionAction
  } = {},
): SessionEvent | null {
  const t = obs.turn ?? turn
  const compressionAction = opts.compressionAction ?? (() => undefined)

  switch (obs.kind) {
    case "page_out":
      return withCategory({
        kind: "page_out" as const,
        turn: t,
        action: compressionAction(obs.action),
        summary: obs.summary,
        tier_hint: obs.tier_hint ?? "durable",
        message_count: Array.isArray(obs.archived) ? obs.archived.length : 0,
      })
    case "compressed": {
      const latest = opts.latestSeq ?? -1
      const start = opts.nextArchiveStart ?? 0
      if (latest < start) return null
      return withCategory({
        kind: "compressed" as const,
        turn: t,
        archived_seq_range: [start, latest] as [number, number],
        action: compressionAction(obs.action),
        summary: obs.summary,
        summary_tokens: obs.summary ? Math.max(1, Math.ceil(obs.summary.length / 4)) : undefined,
        archive_ref: opts.archiveRef,
        preserved_refs: opts.preservedRefs ?? [],
      })
    }
    case "renewed":
      return withCategory({
        kind: "context_renewed" as const,
        turn: t,
        sprint: obs.sprint ?? 0,
        handoff_ref: "",
      })
    case "rollbacked":
      return withCategory({
        kind: "rollbacked" as const,
        turn: t,
        checkpoint_history_len: obs.checkpoint_history_len ?? 0,
        reason: obs.reason as RollbackReason | undefined,
      })
    case "capability_changed":
      return withCategory({
        kind: "capability_changed" as const,
        turn: t,
        added: obs.added ?? [],
        removed: obs.removed ?? [],
        ...(obs.change_kind != null && { change_kind: obs.change_kind }),
        ...(obs.capability_id != null && { capability_id: obs.capability_id }),
        ...(obs.version != null && { version: obs.version }),
        ...(obs.mounted_by != null && { mounted_by: obs.mounted_by }),
        ...(obs.mount_reason != null && { mount_reason: obs.mount_reason }),
      })
    case "milestone_advanced":
      return withCategory({
        kind: "milestone_advanced" as const,
        turn: t,
        phase_id: obs.phase_id ?? "",
        capabilities_unlocked: obs.capabilities_unlocked ?? [],
      })
    case "milestone_blocked":
      return withCategory({
        kind: "milestone_blocked" as const,
        turn: t,
        phase_id: obs.phase_id ?? "",
        reason: typeof obs.reason === "string" ? obs.reason : "",
      })
    case "milestone_evidence":
      return withCategory({
        kind: "milestone_evidence" as const,
        turn: t,
        phase_id: obs.phase_id ?? "",
        evidence: obs.evidence ?? [],
      })
    case "checkpoint_taken":
      return withCategory({
        kind: "checkpoint_taken" as const,
        turn: t,
        history_len: obs.history_len ?? 0,
      })
    case "agent_process_changed":
      return withCategory({
        kind: "agent_process_changed" as const,
        turn: t,
        agent_id: obs.agent_id ?? "",
        parent_session_id: obs.parent_session_id ?? "",
        role: obs.role ?? "",
        isolation: obs.isolation ?? "",
        context_inheritance: obs.context_inheritance ?? "",
        state: (obs as { state?: string }).state ?? "running",
        permitted_capability_ids: obs.permitted_capability_ids ?? [],
        ...((obs as { result_termination?: string }).result_termination
          ? { result_termination: (obs as { result_termination?: string }).result_termination }
          : {}),
      })
    case "tool_gated":
      return withCategory({
        kind: "tool_gated" as const,
        turn: t,
        call_id: obs.call_id ?? "",
        tool: obs.tool ?? "",
        reason: typeof obs.reason === "string" ? obs.reason : "",
      })
    case "signal_disposed":
      return withCategory({
        kind: "signal_disposed" as const,
        turn: t,
        signal_id: obs.signal_id ?? "",
        disposition: obs.disposition ?? "",
        queue_depth: obs.queue_depth ?? 0,
      })
    case "budget_exceeded":
      return withCategory({
        kind: "budget_exceeded" as const,
        turn: t,
        budget: obs.budget ?? "",
      })
    case "suspended":
      return withCategory({
        kind: "suspended" as const,
        turn: t,
        reason: typeof obs.reason === "string" ? obs.reason : "",
        pending_calls: obs.pending_calls ?? [],
      })
    case "resumed":
      return withCategory({
        kind: "resumed" as const,
        turn: t,
        approved: obs.approved ?? [],
        denied: obs.denied ?? [],
      })
    case "page_in_requested":
      return null
    default:
      return null
  }
}
