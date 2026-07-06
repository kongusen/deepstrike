/**
 * L1 — Sourced Q&A assistant.
 *
 * The smallest real agent: a `RuntimeRunner` wired to a provider, a `LocalExecutionPlane` holding
 * the studio's `search` / `read_source` tools, and a `FileSessionLog` so a run is durable and
 * resumable. Ask a question; the agent searches the index, reads the relevant sources, and answers
 * with citations.
 *
 * Mechanisms introduced here (reused by every later level):
 *   • Tools + Execution Plane — the agent's only way to affect the world, gated by the kernel.
 *   • Provider — the real LLM behind the loop.
 *   • Session log / replay & recovery — interrupt a run, re-run with the same --session id, and the
 *     kernel replays the transcript and continues instead of restarting.
 *
 * Run:
 *   npm install                                   # once, links the local SDK + tsx
 *   npm run build --prefix ../node                # build the SDK dist the examples import
 *   ANTHROPIC_API_KEY=sk-... npx tsx 01-sourced-qa/main.ts "How does prompt caching work?"
 *   npx tsx 01-sourced-qa/main.ts --dry-run       # validate wiring, no key, no call
 *
 * Resume demo: run once, Ctrl-C mid-answer, then re-run the SAME command with
 *   --session my-run   (a FileSessionLog persists the transcript under .sessions/)
 */
import { RuntimeRunner, LocalExecutionPlane, FileSessionLog } from "@deepstrike/sdk"
import type { StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent } from "@deepstrike/sdk"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"

const here = dirname(fileURLToPath(import.meta.url))

async function main(): Promise<void> {
  loadEnv()
  const { positionals, flags } = parseArgs(process.argv.slice(2))
  const goal = positionals.join(" ") || "How does prompt caching stay effective across turns? Cite your sources."
  const sessionId = typeof flags.session === "string" ? flags.session : "l1-sourced-qa"
  const dryRun = flags["dry-run"] === true

  // The execution plane owns the tools; the kernel approves each call before the plane runs it.
  const plane = new LocalExecutionPlane()
  for (const t of studioTools()) plane.register(t)
  // A file-backed log makes the session durable: the same sessionId resumes across process runs.
  const sessionLog = new FileSessionLog(join(here, ".sessions"))

  if (dryRun) {
    console.log("● L1 wiring check (no provider call)")
    console.log(`  session id : ${sessionId}`)
    console.log(`  session log: ${join(here, ".sessions")}/${sessionId}.jsonl`)
    console.log(`  tools      : ${studioTools().map((t) => t.schema.name).join(", ")}`)
    console.log(`  goal       : ${goal}`)
    console.log("  ✓ runner constructs; set ANTHROPIC_API_KEY and drop --dry-run to run it live.")
    return
  }

  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog,
    maxTokens: 200_000,
    maxTurns: 12,
  })

  // If this session id already has a transcript, run() detects it and RESUMES (continues the DAG /
  // reasoning) instead of starting over — the recovery mechanism, for free.
  const prior = await sessionLog.read(sessionId)
  if (prior.length > 0) console.log(`↻ resuming session '${sessionId}' (${prior.length} prior events)\n`)

  for await (const event of runner.run({ sessionId, goal })) {
    render(event)
  }
  console.log() // trailing newline
}

// StreamEvent is a base interface (`type: string`); the concrete events extend it, so we branch on
// `type` and cast to the matching subinterface — the same idiom the SDK's own consumers use.
function render(event: StreamEvent): void {
  switch (event.type) {
    case "text_delta":
      process.stdout.write((event as TextDelta).delta)
      break
    case "tool_call": {
      const e = event as ToolCallEvent
      const arg = e.arguments ? Object.values(e.arguments)[0] : ""
      process.stdout.write(`\n  [→ ${e.name}(${JSON.stringify(arg)})]\n`)
      break
    }
    case "tool_result": {
      const e = event as ToolResultEvent
      const preview = e.content.slice(0, 100).replace(/\s+/g, " ")
      process.stdout.write(`  [← ${preview}${e.content.length > 100 ? "…" : ""}]\n`)
      break
    }
    case "done": {
      const e = event as DoneEvent
      process.stdout.write(`\n\n[done: ${e.status} · ${e.iterations} turns · ~${e.totalTokens} tokens]`)
      break
    }
  }
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
