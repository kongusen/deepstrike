/**
 * L4 — Reactive desk: signals + the attention policy.
 *
 * L1's agent, now OPEN to the outside world. Two inbound channels feed events into a running loop;
 * both drain at a turn boundary and route through the kernel's attention policy (queue /
 * soft-interrupt / preempt by urgency):
 *
 *   • SignalGateway (external) — a webhook / cron / upstream job calls `gateway.ingest(signal)`.
 *     The gateway is the `signalSource`; the loop pulls the next signal each turn. Here a "wire
 *     alert" is ingested the first time the agent searches — a real external event arriving mid-run.
 *
 *   • injectNote (host) — `runner.injectNote(text, urgency)` pushes a contextual note on the same
 *     channel without wiring a full source. Here the host fires a `"high"` editor's note the first
 *     time the agent reads a source; `"high"` soft-interrupts (vs `"normal"` queue, `"critical"`
 *     preempt). The note surfaces to the model as a `[SIGNAL] …` line.
 *
 * To make the demo deterministic, both events fire as SIDE EFFECTS of the agent's own tool calls
 * (so they land mid-run every time, no wall-clock race). In production they'd come from a webhook
 * handler and a host monitor — the wiring the agent sees is identical.
 *
 * New mechanism: Signals + reactive attention. Reused: tools, execution plane, provider (L1).
 *
 * Run:  npx tsx 04-reactive-desk/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog } from "@deepstrike/sdk"
import type { RegisteredTool } from "@deepstrike/sdk"
import { SignalGateway } from "@deepstrike/sdk/os" // the external-events entry point lives in the OS surface
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"
import { render } from "../shared/render.js"

/** Wrap a tool so `effect()` fires once, when it is first invoked — a deterministic stand-in for an
 *  external event that happens to arrive while the agent is mid-task. Delegates to the base tool
 *  unchanged (preserving its exact return type, streaming tools included). */
function onceAfter(base: RegisteredTool, effect: () => void): RegisteredTool {
  let fired = false
  return {
    ...base,
    execute(args, ctx) {
      if (!fired) {
        fired = true
        effect()
      }
      return base.execute(args, ctx)
    },
  }
}

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const gateway = new SignalGateway()
  let runner!: RuntimeRunner // late-bound so tool side effects can reach injectNote

  const [search, readSource] = studioTools()
  const plane = new LocalExecutionPlane()
  // search → an external wire alert lands via the gateway (source="gateway", normal ⇒ queues).
  plane.register(
    onceAfter(search, () =>
      gateway.ingest({
        source: "gateway",
        signalType: "alert",
        urgency: "normal",
        payload: { goal: "Wire alert (webhook): a correction to the signals source just landed — treat src-signals as freshly revised." },
        dedupeKey: "wire-correction",
      }),
    ),
  )
  // read_source → the host injects a HIGH-urgency editor's note (soft-interrupt).
  plane.register(
    onceAfter(readSource, () =>
      runner.injectNote(
        "Editor's note: name the attention-policy ladder explicitly — queue (normal) / soft-interrupt (high) / preempt (critical).",
        "high",
      ),
    ),
  )

  if (dryRun) {
    console.log("● L4 wiring check (no provider call)")
    console.log(`  signal source : SignalGateway (implements SignalSource; pulled each turn)`)
    console.log(`  channel 1     : gateway.ingest(...)  → external event (fires on first search, normal ⇒ queue)`)
    console.log(`  channel 2     : runner.injectNote(..., "high")  → host note (fires on first read, high ⇒ soft-interrupt)`)
    console.log(`  ladder        : normal=queue · high=soft-interrupt · critical=preempt`)
    console.log("  ✓ both drain at a turn boundary and surface as [SIGNAL] lines to the model.")
    return
  }

  runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog: new InMemorySessionLog(),
    signalSource: gateway, // the gateway IS the source the loop pulls from each turn
    maxTokens: 200_000,
    maxTurns: 14,
  })

  console.log("━━ reactive brief ━━ (events arrive mid-run; watch for [SIGNAL] lines in the reasoning)\n")
  for await (const event of runner.run({
    sessionId: "l4-reactive",
    goal:
      "Using ONLY the studio index, write a short brief on how external events reach an agent. Search first, then " +
      "read the most relevant source. If any wire alerts or editor's notes arrive while you work, acknowledge them " +
      "and fold them into the brief. Cite the source id.",
  })) {
    render(event)
  }

  gateway.destroy()
  console.log(
    `\nTwo events reached a running loop: an external gateway alert (queued) and a high-urgency host ` +
      `note (soft-interrupt). Both drained at a turn boundary through the kernel's attention policy — ` +
      `the agent never had to poll.`,
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
