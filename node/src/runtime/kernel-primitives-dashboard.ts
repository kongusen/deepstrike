import type { SessionEvent } from "./session-log.js"
import { primitiveForKind } from "./kernel-event-log.js"

export interface PrimitivesStats {
  syscall: {
    toolGatedCount: number
    toolDeniedCount: number
    toolCompletedCount: number
    capabilityChanges: number
  }
  sched: {
    turnCount: number
    suspendedCount: number
    resumedCount: number
    signalDisposedCount: number
    budgetExceededCount: number
    activeProcesses: Set<string>
    processState: Map<string, string>
    lastSuspendReason: string
  }
  mm: {
    compressedCount: number
    pageOutCount: number
    pageInCount: number
    largeResultSpooledCount: number
    totalSpooledBytes: number
    contextRenewedCount: number
  }
}

export class KernelPrimitivesDashboard {
  private stats: PrimitivesStats = {
    syscall: {
      toolGatedCount: 0,
      toolDeniedCount: 0,
      toolCompletedCount: 0,
      capabilityChanges: 0,
    },
    sched: {
      turnCount: 0,
      suspendedCount: 0,
      resumedCount: 0,
      signalDisposedCount: 0,
      budgetExceededCount: 0,
      activeProcesses: new Set(),
      processState: new Map(),
      lastSuspendReason: "",
    },
    mm: {
      compressedCount: 0,
      pageOutCount: 0,
      pageInCount: 0,
      largeResultSpooledCount: 0,
      totalSpooledBytes: 0,
      contextRenewedCount: 0,
    },
  }

  constructor(private sessionId: string) {}

  /**
   * Ingest a SessionEvent to update the metrics dashboard.
   */
  ingest(event: SessionEvent): void {
    const prim = primitiveForKind(event.kind)
    if ("turn" in event && event.turn !== undefined) {
      this.stats.sched.turnCount = Math.max(this.stats.sched.turnCount, event.turn)
    }

    if (prim === "syscall") {
      if (event.kind === "tool_gated") this.stats.syscall.toolGatedCount++
      else if (event.kind === "tool_denied") this.stats.syscall.toolDeniedCount++
      else if (event.kind === "capability_changed") this.stats.syscall.capabilityChanges++
    } else if (prim === "sched") {
      if (event.kind === "suspended") {
        this.stats.sched.suspendedCount++
        this.stats.sched.lastSuspendReason = event.reason
      } else if (event.kind === "resumed") {
        this.stats.sched.resumedCount++
      } else if (event.kind === "signal_delivery_disposed") {
        this.stats.sched.signalDisposedCount++
      } else if (event.kind === "budget_exceeded") {
        this.stats.sched.budgetExceededCount++
      } else if (event.kind === "agent_process_changed") {
        const state = event.state ?? "running"
        this.stats.sched.processState.set(event.agent_id, state)
        if (state === "running") {
          this.stats.sched.activeProcesses.add(event.agent_id)
        } else {
          this.stats.sched.activeProcesses.delete(event.agent_id)
        }
      }
    } else if (prim === "mm") {
      if (event.kind === "compressed") this.stats.mm.compressedCount++
      else if (event.kind === "page_out") this.stats.mm.pageOutCount++
      else if (event.kind === "page_in") this.stats.mm.pageInCount++
      else if (event.kind === "context_renewed") this.stats.mm.contextRenewedCount++
      else if (event.kind === "large_result_spooled") {
        this.stats.mm.largeResultSpooledCount++
        this.stats.mm.totalSpooledBytes += event.original_size
      }
    }

    // Special handling for tool completion which logs at outer runner
    if (event.kind === "tool_completed") {
      this.stats.syscall.toolCompletedCount += event.results.length
    }
  }

  /**
   * Return a formatted ANSI terminal dashboard representation.
   */
  render(): string {
    const esc = (code: string) => `\x1b[${code}m`
    const reset = esc("0")
    const bold = esc("1")
    const green = esc("32")
    const cyan = esc("36")
    const yellow = esc("33")
    const magenta = esc("35")
    const gray = esc("90")

    const s = this.stats
    const header = `${bold}${cyan}╔══════════════════════════════════════════════════════════════════════╗${reset}`
    const footer = `${bold}${cyan}╚══════════════════════════════════════════════════════════════════════╝${reset}`
    
    const lines = [
      header,
      `${bold}${cyan}║  DeepStrike Kernel Primitives Diagnostics (Session: ${this.sessionId.slice(0, 8)})  ║${reset}`,
      `${bold}${cyan}╠══════════════════════════════════════════════════════════════════════╣${reset}`,
      `║ ${bold}${green}🔧 SYSCALL (Security & Host Tools)${reset}                                   ║`,
      `║   - Tool Gated: ${s.syscall.toolGatedCount.toString().padEnd(10)} - Tool Completed: ${s.syscall.toolCompletedCount.toString().padEnd(10)}          ║`,
      `║   - Tool Denied: ${s.syscall.toolDeniedCount.toString().padEnd(9)} - Cap. Changes: ${s.syscall.capabilityChanges.toString().padEnd(10)}            ║`,
      `║                                                                      ║`,
      `║ ${bold}${yellow}⏰ SCHED (Task Scheduler & Process Table)${reset}                          ║`,
      `║   - Current Turn: ${s.sched.turnCount.toString().padEnd(9)} - Suspended/Resumed: ${s.sched.suspendedCount}/${s.sched.resumedCount}`.padEnd(70) + "║",
      `║   - Active Sub-Agents: ${s.sched.activeProcesses.size.toString().padEnd(4)} - Signals Handled: ${s.sched.signalDisposedCount.toString().padEnd(10)}          ║`,
      `║   - Last Suspend Reason: ${s.sched.lastSuspendReason.slice(0, 35).padEnd(35)}         ║`,
      `║                                                                      ║`,
      `║ ${bold}${magenta}💾 MM (Memory Management & Context Paging)${reset}                          ║`,
      `║   - Compressions: ${s.mm.compressedCount.toString().padEnd(9)} - Page-Outs (Semantic): ${s.mm.pageOutCount.toString().padEnd(10)}       ║`,
      `║   - Page-Ins (Cache): ${s.mm.pageInCount.toString().padEnd(7)} - Large Spooled: ${s.mm.largeResultSpooledCount.toString().padEnd(14)} ║`,
      `║   - Total Spooled Bytes: ${(s.mm.totalSpooledBytes / 1024).toFixed(1).toString() + " KB"}`.padEnd(70) + "║",
      footer
    ]

    return lines.join("\n")
  }

  /**
   * Log the dashboard to the console.
   */
  print(): void {
    console.log(this.render())
  }
}
