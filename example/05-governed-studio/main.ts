/**
 * L5 — Governed studio: the control plane made explicit.
 *
 * L1's agent, now behind a policy. Three control-plane mechanisms sit between the model and the
 * world, all declarative and kernel-enforced:
 *
 *   • GOVERNANCE (allow / deny / ask_user). A `governancePolicy` classifies each tool. `deny` tools
 *     are pre-filtered OUT of the schema — the model never sees `publish_public`, so it can't even
 *     try. `ask_user` tools reach the model but pause at CALL time: `email_editor` raises a
 *     PermissionRequestEvent that the host (`onPermissionRequest`) adjudicates.
 *
 *   • RESOURCE QUOTA. A `resourceQuota` bounds spawn concurrency / depth / cumulative sub-agents and
 *     the memory-write rate — the hard caps the kernel enforces regardless of what the model asks.
 *
 *   • OS PROFILE snapshot. `osProfile("native")` resolves the concrete kernel-owned policy defaults;
 *     after the run, `rebuildOsSnapshotFromSessionEvents` reconstructs what the kernel actually
 *     enforced (tool-gated count, signals, memory ops) from the durable session log — an audit trail.
 *
 * New mechanisms: Governance, Resource quota, OS profile. Reused: tools, execution plane, provider.
 *
 * Run:  npx tsx 05-governed-studio/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog, tool } from "@deepstrike/sdk"
import type { RegisteredTool, PermissionRequestEvent, PermissionResponse } from "@deepstrike/sdk"
import { osProfile, rebuildOsSnapshotFromSessionEvents } from "@deepstrike/sdk/os"
import type { ResourceQuota } from "@deepstrike/sdk/os"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"
import { render } from "../shared/render.js"

/** Notify the internal editor a brief is ready. Governed as `ask_user`: the host adjudicates. */
function emailEditorTool(): RegisteredTool {
  return tool(
    "email_editor",
    "Notify a recipient that the brief is ready. Args: { to, summary }. Governed — the host approves it.",
    {
      type: "object",
      properties: { to: { type: "string" }, summary: { type: "string" } },
      required: ["to", "summary"],
    },
    (args) => `✓ editor notified (${args.to}). The brief is delivered — the task is COMPLETE, do not notify again.`,
  )
}

/** Publish the brief to the public site. DENIED by policy — pre-filtered from the schema, so the
 *  model never sees it. The handler is here only to prove the deny happens at the policy layer. */
function publishPublicTool(): RegisteredTool {
  return tool(
    "publish_public",
    "Publish the brief to the public website (irreversible).",
    { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
    () => "PUBLISHED (this should be unreachable — the policy denies this tool)",
  )
}

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const tools = [...studioTools(), emailEditorTool(), publishPublicTool()]
  const plane = new LocalExecutionPlane()
  for (const t of tools) plane.register(t)

  // Declarative policy: default allow, one hard deny, one ask_user gate.
  const governancePolicy = {
    defaultAction: "allow" as const,
    rules: [
      { pattern: "publish_public", action: "deny" as const }, // schema pre-filtered → invisible
      { pattern: "email_editor", action: "ask_user" as const }, // pauses for host adjudication
    ],
  }
  // Hard caps the kernel enforces no matter what the model plans (no sub-agents here, but the caps
  // are lowered into the run and would bound L7/L8's fan-out).
  const resourceQuota: ResourceQuota = { maxConcurrentSubagents: 2, maxTotalSubagents: 4, maxSpawnDepth: 2 }

  // The host's adjudicator for every ask_user gate. The gate is TOOL-SCOPED (the kernel surfaces the
  // tool name + reason, not the call args), so the host decides per capability: `email_editor` is the
  // studio's own notification tool → approve; anything else escalated → refuse. A one-line policy the
  // MODEL cannot override — approval authority lives with the host, not the prompt.
  const onPermissionRequest = (e: PermissionRequestEvent): PermissionResponse => {
    const approved = e.toolName === "email_editor"
    return { approved, responder: "studio-host", reason: approved ? "studio notification tool" : `unapproved capability '${e.toolName}'` }
  }

  const profile = osProfile("native")
  if (dryRun) {
    console.log("● L5 wiring check (no provider call)")
    console.log(`  base tools     : ${tools.map((t) => t.schema.name).join(", ")}`)
    console.log(`  governance     : deny publish_public (invisible) · ask_user email_editor (host-adjudicated)`)
    console.log(`  resource quota : ${JSON.stringify(resourceQuota)}`)
    console.log(`  os profile     : ${profile.id}  · governance ${JSON.stringify(profile.governancePolicy.rules)}`)
    console.log("  ✓ set a key and drop --dry-run to watch deny + ask_user gates fire.")
    return
  }

  // In-memory log: each run starts clean (this level teaches governance, not resume — see L1), yet
  // we can still rebuild the OS snapshot from the events it captured this run.
  const sessionLog = new InMemorySessionLog()
  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog,
    governancePolicy,
    resourceQuota,
    osProfile: "native",
    onPermissionRequest,
    maxTokens: 200_000,
    maxTurns: 14,
  })

  console.log("━━ governed run ━━ (publish_public is denied & invisible; email_editor pauses for the host)\n")
  const sessionId = "l5-governed"
  for await (const event of runner.run({
    sessionId,
    goal:
      "Using ONLY the studio index, write a ONE-sentence brief on how memory writes are governed (cite the id). " +
      "Then call email_editor EXACTLY ONCE with to='editor' to notify them, and stop. " +
      "Do NOT publish it publicly and do NOT notify more than once.",
  })) {
    render(event)
  }

  // OS profile snapshot: reconstruct what the kernel actually enforced, from the durable log.
  const logged = await sessionLog.read(sessionId)
  const events = logged.map((e) => e.event)
  const snap = rebuildOsSnapshotFromSessionEvents(events)
  console.log(`\n━━ OS snapshot (rebuilt from ${events.length} session events) ━━`)
  console.log(`  tool-gated (ask_user) : ${snap.toolGatedCount}`)
  console.log(`  memory written        : ${snap.memoryWrittenCount}`)
  console.log(`  signals routed        : ${snap.signals.length}`)
  console.log(
    "\npublish_public never appeared in the toolset (policy deny → schema pre-filter); email_editor " +
      "reached the model but the HOST decided whether it fired. The control plane, not the prompt, is authority.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
