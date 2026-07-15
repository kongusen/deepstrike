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
    case "large_result_spooled":
    case "memory_written":
    case "memory_queried":
    case "memory_validation_failed":
      return "mm"
    case "agent_process_changed":
      return "proc"
    case "signal_delivery_disposed":
      return "ipc"
    default:
      return "sched"
  }
}

type CompressionAction = Extract<SessionEvent, { kind: "compressed" }>["action"]

export function kernelObservationToSessionEvent(
  obs: KernelObservation,
  turn: number,
  opts: {
    nextArchiveStart?: number
    latestSeq?: number
    preservedRefs?: string[]
    compressionAction?: (action?: string) => CompressionAction
  } = {},
): SessionEvent | null {
  const t = obs.turn ?? turn
  const compressionAction = opts.compressionAction ?? (() => undefined)

  switch (obs.kind) {
    case "compressed": {
      const latest = opts.latestSeq ?? -1
      const start = opts.nextArchiveStart ?? 0
      if (latest < start) return null
      return {
        kind: "compressed" as const,
        turn: t,
        archived_seq_range: [start, latest] as [number, number],
        action: compressionAction(obs.action),
        summary: obs.summary,
        summary_tokens: obs.summary ? Math.max(1, Math.ceil(obs.summary.length / 4)) : undefined,
        preserved_refs: opts.preservedRefs ?? [],
      }
    }
    case "renewed":
      return {
        kind: "context_renewed" as const,
        turn: t,
        sprint: obs.sprint ?? 0,
        handoff_ref: "",
      }
    case "rollbacked":
      return {
        kind: "rollbacked" as const,
        turn: t,
        checkpoint_history_len: obs.checkpoint_history_len ?? 0,
        reason: obs.reason as RollbackReason | undefined,
      }
    case "capability_changed":
      return {
        kind: "capability_changed" as const,
        turn: t,
        added: obs.added ?? [],
        removed: obs.removed ?? [],
        ...(obs.change_kind != null && { change_kind: obs.change_kind }),
        ...(obs.capability_id != null && { capability_id: obs.capability_id }),
        ...(obs.version != null && { version: obs.version }),
        ...(obs.mounted_by != null && { mounted_by: obs.mounted_by }),
        ...(obs.mount_reason != null && { mount_reason: obs.mount_reason }),
      }
    case "milestone_advanced":
      return {
        kind: "milestone_advanced" as const,
        turn: t,
        phase_id: obs.phase_id ?? "",
        capabilities_unlocked: obs.capabilities_unlocked ?? [],
      }
    case "milestone_blocked":
      return {
        kind: "milestone_blocked" as const,
        turn: t,
        phase_id: obs.phase_id ?? "",
        reason: typeof obs.reason === "string" ? obs.reason : "",
      }
    case "checkpoint_taken":
      return {
        kind: "checkpoint_taken" as const,
        turn: t,
        history_len: obs.history_len ?? 0,
      }
    case "entropy_sample":
      return {
        kind: "entropy_sample" as const,
        turn: t,
        score: obs.score ?? 0,
        score_version: obs.score_version ?? 0,
        rho: obs.rho ?? 0,
        repeat_pressure: obs.repeat_pressure ?? 0,
        failure_rate: obs.failure_rate ?? 0,
        rollbacks_in_window: obs.rollbacks_in_window ?? 0,
        window_turns: obs.window_turns ?? 0,
      }
    case "entropy_alert":
      return {
        kind: "entropy_alert" as const,
        turn: t,
        score: obs.score ?? 0,
        threshold: obs.threshold ?? 0,
      }
    case "agent_process_changed":
      return {
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
      }
    case "tool_gated":
      return {
        kind: "tool_gated" as const,
        turn: t,
        call_id: obs.call_id ?? "",
        tool: obs.tool ?? "",
        reason: typeof obs.reason === "string" ? obs.reason : "",
      }
    case "signal_delivery_disposed":
      return {
        kind: "signal_delivery_disposed" as const,
        turn: t,
        operation_id: obs.operation_id ?? "",
        delivery_id: obs.delivery_id ?? "",
        attempt: obs.attempt ?? 0,
        signal_id: obs.signal_id ?? "",
        disposition: obs.disposition ?? "",
        queue_depth: obs.queue_depth ?? 0,
      }
    case "budget_exceeded":
      return {
        kind: "budget_exceeded" as const,
        turn: t,
        budget: obs.budget ?? "",
        operation_id: obs.operation_id ?? "",
        ...(obs.reservation_id ? { reservation_id: obs.reservation_id } : {}),
      }
    case "budget_usage_reported":
      return {
        kind: "budget_usage_reported" as const,
        turn: t,
        operation_id: obs.operation_id ?? "",
        reservation_id: obs.reservation_id ?? "",
        tokens: obs.tokens ?? 0,
        subagents: obs.subagents ?? 0,
        rounds: obs.rounds ?? 0,
      }
    case "operation_cancelled":
      return {
        kind: "operation_cancelled" as const,
        turn: t,
        operation_id: obs.operation_id ?? "",
        reason: (obs.reason ?? "user") as "user" | "deadline" | "lease_lost" | "host_shutdown",
        pending_call_ids: obs.pending_call_ids ?? [],
      }
    case "suspended":
      return {
        kind: "suspended" as const,
        turn: t,
        reason: typeof obs.reason === "string" ? obs.reason : "",
        pending_calls: obs.pending_calls ?? [],
      }
    case "resumed":
      return {
        kind: "resumed" as const,
        turn: t,
        approved: obs.approved ?? [],
        denied: obs.denied ?? [],
      }
    case "page_in_requested":
      return null
    case "large_result_spooled":
      return {
        kind: "large_result_spooled" as const,
        turn: t,
        call_id: obs.call_id ?? "",
        tool: obs.tool ?? "",
        original_size: obs.original_size ?? 0,
        preview_size: obs.preview_size ?? 0,
        spool_ref: obs.spool_ref,
      }
    case "page_out_archived":
      return {
        kind: "page_out" as const,
        turn: t,
        action: compressionAction(obs.action),
        summary: obs.summary,
        tier_hint: obs.tier,
        message_count: obs.message_count ?? 0,
        archive_ref: obs.archive_ref,
      }
    case "memory_written":
      return {
        kind: "memory_written" as const,
        turn: t,
        record_id: obs.record_id ?? "",
        scope: obs.scope ?? { tenant_id: "", namespace: "" },
        memory_kind: obs.memory_kind ?? "",
        name: obs.name ?? "",
        size_bytes: obs.size_bytes ?? 0,
      }
    case "memory_queried":
      return {
        kind: "memory_queried" as const,
        turn: t,
        scope: obs.scope ?? { tenant_id: "", namespace: "" },
        query: obs.query ?? "",
        requested_k: obs.requested_k ?? 0,
        requires_async_response: obs.requires_async_response ?? false,
      }
    case "memory_validation_failed":
      return {
        kind: "memory_validation_failed" as const,
        turn: t,
        record_id: obs.record_id ?? "",
        error: obs.error ?? "",
      }
    case "memory_write_failed":
      return {
        kind: "memory_write_failed" as const,
        turn: t,
        record_id: obs.record_id ?? "",
        error: obs.error ?? "",
      }
    case "memory_query_failed":
      return {
        kind: "memory_query_failed" as const,
        turn: t,
        scope: obs.scope ?? { tenant_id: "", namespace: "" },
        query: obs.query ?? "",
        error: obs.error ?? "",
      }
    case "workflow_batch_spawned": {
      // Batch metadata persisted for resume recovery; individual nodes are
      // recorded when they complete (via workflow_node_completed).
      const nodes = (obs as any).nodes ?? []
      return {
        kind: "workflow_batch_spawned" as const,
        turn: t,
        node_count: nodes.length,
        node_ids: nodes.map((n: any) => n.agent_id ?? ""),
      }
    }
    case "workflow_completed": {
      const nodeOutcomes = (obs as any).node_outcomes ?? []
      return {
        kind: "workflow_completed" as const,
        turn: t,
        node_outcomes: nodeOutcomes,
        total_nodes: nodeOutcomes.length,
      }
    }
    default:
      return null
  }
}

export type KernelPrimitive = "syscall" | "sched" | "mm"

export function primitiveForCategory(category: KernelEventCategory): KernelPrimitive {
  switch (category) {
    case "syscall":
      return "syscall"
    case "mm":
      return "mm"
    case "proc":
    case "ipc":
    case "sched":
    default:
      return "sched"
  }
}

export function primitiveForKind(kind: string): KernelPrimitive {
  return primitiveForCategory(categoryForKind(kind))
}
