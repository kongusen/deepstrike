import type { SessionEvent } from "./session-log.js"
import { categoryForKind, type KernelEventCategory, type KernelPrimitive, primitiveForKind } from "./kernel-event-log.js"

const KERNEL_KINDS = new Set([
  "compressed",
  "page_out",
  "page_in",
  "large_result_spooled",
  "capability_changed",
  "context_renewed",
  "suspended",
  "resumed",
  "tool_gated",
  "signal_delivery_disposed",
  "budget_exceeded",
  "budget_usage_reported",
  "checkpoint_taken",
  "rollbacked",
  "agent_process_changed",
  "milestone_advanced",
  "milestone_blocked",
  "memory_written",
  "memory_queried",
  "memory_validation_failed",
])

export interface OsSnapshot {
  lastSuspend?: { turn: number; reason: string; pending_calls: string[] }
  lastResumedTurn?: number
  processByAgent: Array<{ turn: number; agent_id: string; parent_session_id: string; state: string }>
  budgetExceeded: Array<{ turn: number; operation_id: string; reservation_id?: string; budget: string }>
  budgetUsageReported: Array<{
    turn: number
    operation_id: string
    reservation_id: string
    tokens: number
    subagents: number
    rounds: number
  }>
  signals: Array<{
    turn: number
    operation_id: string
    delivery_id: string
    attempt: number
    signal_id: string
    disposition: string
    queue_depth: number
  }>
  pageOutCount: number
  pageInCount: number
  spoolCount: number
  toolGatedCount: number
  memoryWrittenCount: number
  memoryQueriedCount: number
  memoryValidationFailedCount: number
  memoryRetrievalResultCount: number
}

export function rebuildOsSnapshotFromSessionEvents(
  events: SessionEvent[],
): OsSnapshot {
  const snap: OsSnapshot = {
    processByAgent: [],
    budgetExceeded: [],
    budgetUsageReported: [],
    signals: [],
    pageOutCount: 0,
    pageInCount: 0,
    spoolCount: 0,
    toolGatedCount: 0,
    memoryWrittenCount: 0,
    memoryQueriedCount: 0,
    memoryValidationFailedCount: 0,
    memoryRetrievalResultCount: 0,
  }
  const index = new Map<string, number>()

  for (const event of events) {
    if (event.kind === "memory_retrieval_result") {
      snap.memoryRetrievalResultCount += 1
      continue
    }
    if (!KERNEL_KINDS.has(event.kind) && event.kind !== "suspended" && event.kind !== "resumed") {
      continue
    }
    switch (event.kind) {
      case "suspended":
        snap.lastSuspend = {
          turn: event.turn,
          reason: event.reason,
          pending_calls: event.pending_calls ?? [],
        }
        break
      case "resumed":
        snap.lastResumedTurn = event.turn
        break
      case "tool_gated":
        snap.toolGatedCount += 1
        break
      case "agent_process_changed": {
        const record = {
          turn: event.turn,
          agent_id: event.agent_id,
          parent_session_id: event.parent_session_id,
          state: event.state ?? "running",
        }
        const idx = index.get(event.agent_id)
        if (idx !== undefined) snap.processByAgent[idx] = record
        else {
          index.set(event.agent_id, snap.processByAgent.length)
          snap.processByAgent.push(record)
        }
        break
      }
      case "budget_exceeded":
        snap.budgetExceeded.push({
          turn: event.turn,
          operation_id: event.operation_id,
          ...(event.reservation_id ? { reservation_id: event.reservation_id } : {}),
          budget: event.budget,
        })
        break
      case "budget_usage_reported":
        snap.budgetUsageReported.push({
          turn: event.turn,
          operation_id: event.operation_id,
          reservation_id: event.reservation_id,
          tokens: event.tokens,
          subagents: event.subagents,
          rounds: event.rounds,
        })
        break
      case "signal_delivery_disposed":
        snap.signals.push({
          turn: event.turn,
          operation_id: event.operation_id,
          delivery_id: event.delivery_id,
          attempt: event.attempt,
          signal_id: event.signal_id,
          disposition: event.disposition,
          queue_depth: event.queue_depth,
        })
        break
      case "page_out":
        snap.pageOutCount += 1
        break
      case "page_in":
        snap.pageInCount += 1
        break
      case "large_result_spooled":
        snap.spoolCount += 1
        break
      case "memory_written":
        snap.memoryWrittenCount += 1
        break
      case "memory_queried":
        snap.memoryQueriedCount += 1
        break
      case "memory_validation_failed":
        snap.memoryValidationFailedCount += 1
        break
      default:
        break
    }
  }
  return snap
}
