import "dotenv/config"
import * as readline from "readline"
import { makeRuntime } from "./runtime.js"
import { makeProvider } from "./provider.js"
import { makeNoteJudge, NOTE_CRITERIA } from "./harness/note_judge.js"
import { makeContributionJudge, CONTRIBUTION_CRITERIA } from "./harness/contribution_judge.js"
import { makeReportJudge, REPORT_CRITERIA } from "./harness/report_judge.js"
import { parseCommand, HELP_TEXT } from "./signals/cli_bridge.js"
import { startInboxWatcher } from "./signals/inbox_watcher.js"
import { saveNote, parseNoteOutput, generateId } from "./archive.js"
import { makeFileDreamStore } from "./memory/dream_store.js"
import { webSearch, fetchAndClip, searchArchive, exportTool, exportDataset } from "./tools/index.js"
import { readFile } from "fs/promises"
import { INBOX_DIR } from "./paths.js"
import type { TextDelta, DoneEvent, PermissionRequestEvent } from "@deepstrike/sdk"

// ─── helpers ─────────────────────────────────────────────────────────────────

const criteria = (items: string[]) => items.map(text => ({ text, required: true }))

function banner(msg: string) {
  console.log(`\n\x1b[36m── ${msg} ${"─".repeat(Math.max(0, 52 - msg.length))}\x1b[0m`)
}

function ok(msg: string) { console.log(`\x1b[32m✓\x1b[0m ${msg}`) }
function warn(msg: string) { console.log(`\x1b[33m⚠\x1b[0m ${msg}`) }
function err(msg: string) { console.log(`\x1b[31m✗\x1b[0m ${msg}`) }

async function streamRun(label: string, agent: ReturnType<typeof makeRuntime>): Promise<string> {
  banner(label)
  let text = ""
  for await (const evt of agent.runStreaming(label)) {
    if (evt.type === "text_delta") {
      process.stdout.write((evt as TextDelta).delta)
      text += (evt as TextDelta).delta
    } else if (evt.type === "permission_request") {
      const pr = evt as PermissionRequestEvent
      warn(`governance: ${pr.toolName} requires approval — skipped (${pr.reason})`)
    } else if (evt.type === "done") {
      const d = evt as DoneEvent
      process.stdout.write("\n")
      ok(`done · ${d.iterations} turn(s) · ${d.totalTokens} tokens · status: ${d.status}`)
    }
  }
  return text
}

// ─── capture a personal note ─────────────────────────────────────────────────

async function processCapture(raw: string, source: "personal" | "community" = "personal", contributor?: string) {
  const agent = makeRuntime("capture")
  agent.register(searchArchive, fetchAndClip, exportTool)

  const goal = `Organize this note into the FlashNote knowledge base.
Use the classify_and_tag skill, then call search_archive to find related notes.
Output strictly a JSON object (no extra text):

{
  "type": "idea|article|task|reference|insight|research",
  "tags": ["#tag1", "#tag2"],
  "summary": "one-line summary ≤80 chars",
  "connections": ["note_id if found, else empty"]
}

Note to organize:
${raw}`

  const harness = makeNoteJudge(agent)
  const outcome = await harness.run({ goal, criteria: criteria(NOTE_CRITERIA) })

  const note = parseNoteOutput(outcome.result, raw, source, contributor)
  if (note) {
    if (!outcome.passed) note.qualityScore = 0.6
    await saveNote(note)
    ok(`saved → archive/${note.id}.json  [q=${note.qualityScore}]`)
    console.log(`  type: ${note.type}  tags: ${note.tags.join(" ")}`)
    console.log(`  summary: ${note.summary}`)
    if (note.connections.length) console.log(`  connections: ${note.connections.join(", ")}`)
  } else {
    warn("could not parse note output — saved raw")
    const fallback = {
      id: generateId(), type: "idea" as const, tags: ["#unprocessed"],
      summary: raw.slice(0, 80), connections: [], source, contributor,
      raw, qualityScore: 0.3, createdAt: Date.now(),
    }
    await saveNote(fallback)
  }
}

// ─── process a file from inbox ───────────────────────────────────────────────

async function processFile(filePath: string) {
  banner(`inbox: ${filePath}`)
  try {
    const content = await readFile(filePath, "utf8")
    await processCapture(content.slice(0, 4000))
  } catch (e) {
    err(`failed to read ${filePath}: ${String(e)}`)
  }
}

// ─── deep research ───────────────────────────────────────────────────────────

async function processResearch(topic: string) {
  banner(`research: ${topic}`)
  const agent = makeRuntime("research")
  agent.register(webSearch, fetchAndClip, searchArchive, exportTool)

  const goal = `Research this topic thoroughly: "${topic}"

Steps:
1. Use the outline_research skill to create a research plan
2. Call web_search with 2–3 queries from the plan
3. Call fetch_and_clip on the most promising URLs
4. Use summarize_source skill after each fetch
5. Call search_archive to cross-reference with existing notes
6. Write a comprehensive research report with:
   - TL;DR (2–3 sentences)
   - Key findings with source URLs
   - Comparison or analysis
   - Conclusion
   - References list

All claims must cite a URL or archive note ID.`

  const harness = makeReportJudge(agent, makeProvider())
  let report = ""
  let passed = false
  for await (const event of harness.runStreaming({ goal, criteria: criteria(REPORT_CRITERIA) })) {
    if (event.type === "token") {
      process.stdout.write(event.text)
      report += event.text
    } else if (event.type === "done") {
      passed = event.verdict.passed
      process.stdout.write("\n")
    }
  }

  // Save as research note
  const note = {
    id: generateId(),
    type: "research" as const,
    tags: [`#research`, `#${topic.toLowerCase().replace(/\s+/g, "-").slice(0, 30)}`],
    summary: `Research: ${topic.slice(0, 60)}`,
    connections: [],
    source: "personal" as const,
    raw: report,
    qualityScore: passed ? 0.9 : 0.7,
    createdAt: Date.now(),
  }
  await saveNote(note)
  ok(`research saved → archive/${note.id}.json  [passed=${passed}]`)
}

// ─── structured interview ────────────────────────────────────────────────────

async function processInterview(contributor: string, topic: string) {
  banner(`interview: ${contributor} on "${topic}"`)
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout })
  const ask = (q: string) => new Promise<string>(res => rl.question(q, res))

  const agent = makeRuntime("capture")
  agent.register(searchArchive)

  const exchanges: string[] = []
  let round = 0

  console.log(`\nStarting interview with ${contributor} about: ${topic}`)
  console.log("The agent will ask questions. Type responses and press Enter.")
  console.log("Type 'done' to finish early.\n")

  // Generate first question
  let question = `Tell me about your experience with: ${topic}`
  console.log(`\n\x1b[36mQ:\x1b[0m ${question}`)

  while (round < 8) {
    const answer = await ask("\x1b[33mA:\x1b[0m ")
    if (answer.trim().toLowerCase() === "done") break
    exchanges.push(`Q: ${question}\nA: ${answer}`)
    round++

    // Use agent to generate next question
    const nextQ = await agent.run(
      `Interview context so far:\n${exchanges.join("\n\n")}\n\nUsing the elicit_insight skill, generate the next question.`,
    )

    if (nextQ.includes("[DONE]") || round >= 7) break
    question = nextQ.trim()
    console.log(`\n\x1b[36mQ:\x1b[0m ${question}`)
  }

  rl.close()

  if (!exchanges.length) { warn("No exchanges captured."); return }

  // Process the full interview as a community contribution
  const raw = `Interview with ${contributor} on "${topic}"\n\n${exchanges.join("\n\n")}`
  banner("processing interview")

  const processAgent = makeRuntime("capture")
  processAgent.register(searchArchive, exportTool)

  const goal = `Extract and organize insights from this interview.
Use the classify_and_tag skill.
Output JSON with type="insight", tags, summary, connections, and a body field containing the key insights.

${raw}`

  const harness = makeContributionJudge(processAgent)
  const outcome = await harness.run({ goal, criteria: criteria(CONTRIBUTION_CRITERIA) })

  const note = parseNoteOutput(outcome.result, raw, "community", contributor)
  if (note) {
    await saveNote(note)
    ok(`interview saved → archive/${note.id}.json  [passed=${outcome.passed}]`)
  } else {
    const fallback = {
      id: generateId(), type: "insight" as const,
      tags: ["#interview", `#${contributor}`],
      summary: `Interview: ${contributor} on ${topic.slice(0, 40)}`,
      connections: [], source: "community" as const, contributor,
      raw, qualityScore: 0.5, createdAt: Date.now(),
    }
    await saveNote(fallback)
    ok(`saved as raw interview → archive/${fallback.id}.json`)
  }
}

// ─── export ──────────────────────────────────────────────────────────────────

async function processExport(format: string) {
  banner(`export: ${format}`)
  const agent = makeRuntime("capture")
  agent.register(exportTool)
  const result = await agent.run(`Call the export tool with format="${format}".`)
  console.log(result)
}

async function processExportDataset(format: string, minQuality: number, anonymize: boolean) {
  banner(`export-dataset: ${format} (min_quality=${minQuality}${anonymize ? ", anonymize" : ""})`)
  const agent = makeRuntime("capture")
  agent.register(exportDataset)
  const result = await agent.run(
    `Call export_dataset with format="${format}", min_quality=${minQuality}, anonymize=${anonymize}.`,
  )
  console.log(result)
}

// ─── cluster & synthesize ────────────────────────────────────────────────────

async function processCluster(topic: string) {
  banner(`cluster: ${topic}`)
  const agent = makeRuntime("capture")
  agent.register(searchArchive, exportTool)
  const result = await agent.run(
    `Find notes related to "${topic}" using search_archive, then use the synthesize_cluster skill to fuse them into a single insight note. Output the insight JSON.`,
  )
  const note = parseNoteOutput(result, `Cluster synthesis: ${topic}`, "personal")
  if (note) {
    note.type = "insight"
    await saveNote(note)
    ok(`cluster insight saved → archive/${note.id}.json`)
  }
  console.log(result.slice(0, 600))
}

// ─── graceful shutdown ───────────────────────────────────────────────────────

async function shutdown() {
  banner("shutting down — saving memory")
  try {
    const runtime = makeRuntime("capture")
    const result = await runtime.dream("flashnote")
    ok(`dream complete: +${result.entriesAdded} memories, ${result.sessionsProcessed} sessions processed`)
  } catch (e) {
    warn(`dream skipped: ${String(e)}`)
  }
  process.exit(0)
}

// ─── main loop ───────────────────────────────────────────────────────────────

async function main() {
  console.log("\x1b[1m\x1b[35mFlashNote\x1b[0m — 闪念整理助手")
  console.log("Type /help for available commands\n")

  if (!process.env.OPENAI_API_KEY) {
    err("OPENAI_API_KEY not set. Copy .env.example to .env and fill in your key.")
    process.exit(1)
  }

  // Start inbox watcher
  const watcherTimer = startInboxWatcher(INBOX_DIR, async (paths) => {
    for (const p of paths) await processFile(p)
  })

  process.on("SIGINT", async () => { clearInterval(watcherTimer); await shutdown() })
  process.on("SIGTERM", async () => { clearInterval(watcherTimer); await shutdown() })

  const rl = readline.createInterface({ input: process.stdin, terminal: true })
  process.stdout.write("\x1b[33m> \x1b[0m")

  rl.on("line", async (line) => {
    const cmd = parseCommand(line)

    switch (cmd.type) {
      case "help":
        console.log(HELP_TEXT); break

      case "dump":
        await processCapture(cmd.text); break

      case "research":
        await processResearch(cmd.topic); break

      case "interview":
        await processInterview(cmd.contributor, cmd.topic); break

      case "export":
        await processExport(cmd.format); break

      case "export_dataset":
        await processExportDataset(cmd.format, cmd.minQuality, cmd.anonymize); break

      case "cluster":
        await processCluster(cmd.topic); break

      case "stop":
        clearInterval(watcherTimer)
        await shutdown(); break

      case "unknown":
        warn(`Unknown command: ${cmd.raw}\nType /help for available commands.`); break
    }

    process.stdout.write("\x1b[33m> \x1b[0m")
  })
}

main().catch((e) => { err(String(e)); process.exit(1) })
