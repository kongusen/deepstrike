import "dotenv/config"
import { createServer, type IncomingMessage, type ServerResponse } from "http"
import { readFile } from "fs/promises"
import { resolve } from "path"
import { ROOT } from "./paths.js"
import { makeRuntime } from "./runtime.js"
import { makeProvider } from "./provider.js"
import { makeReportJudge, REPORT_CRITERIA } from "./harness/report_judge.js"
import { webSearch, fetchAndClip, searchArchive, exportTool, exportDataset } from "./tools/index.js"
import { saveNote, parseNoteOutput, generateId, loadNotes } from "./archive.js"
import type { TextDelta, DoneEvent } from "@deepstrike/sdk"

const PORT = parseInt(process.env.PORT ?? "3000")

// ─── body / SSE ──────────────────────────────────────────────────────────────

async function readBody(req: IncomingMessage): Promise<Record<string, unknown>> {
  return new Promise((res, rej) => {
    let s = ""
    req.on("data", c => (s += c))
    req.on("end", () => { try { res(JSON.parse(s || "{}")) } catch { res({}) } })
    req.on("error", rej)
  })
}

function sse(res: ServerResponse) {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
    "Access-Control-Allow-Origin": "*",
  })
  return {
    send: (d: object) => res.write(`data: ${JSON.stringify(d)}\n\n`),
    done: () => { res.write("data: [DONE]\n\n"); res.end() },
  }
}

function cors(res: ServerResponse) {
  res.setHeader("Access-Control-Allow-Origin", "*")
  res.setHeader("Access-Control-Allow-Headers", "Content-Type")
}

// ─── agent streaming helper ──────────────────────────────────────────────────

async function streamAgent(
  agent: ReturnType<typeof makeRuntime>,
  goal: string,
  criteria: string[],
  send: (d: object) => void,
): Promise<string> {
  let text = ""
  for await (const evt of agent.runStreaming(goal, criteria)) {
    if (evt.type === "text_delta") {
      const delta = (evt as TextDelta).delta
      text += delta
      send({ type: "delta", content: delta })
    } else if (evt.type === "tool_result") {
      const e = evt as { name?: string; content?: string }
      if (e.name === "skill") {
        const m = String(e.content ?? "").match(/^---\s*\nname:\s*(\S+)/m)
        send({ type: "skill", content: m?.[1] ?? "unknown" })
      } else {
        send({ type: "tool", name: e.name ?? "", content: String(e.content ?? "").slice(0, 200) })
      }
    } else if (evt.type === "permission_request") {
      const e = evt as { toolName?: string; reason?: string }
      send({ type: "warn", content: `governance: ${e.toolName} needs approval — skipped` })
    } else if (evt.type === "done") {
      const d = evt as DoneEvent
      send({ type: "done", iterations: d.iterations, tokens: d.totalTokens, status: d.status })
    }
  }
  return text
}

// ─── handlers ────────────────────────────────────────────────────────────────

async function handleCapture(body: Record<string, unknown>, res: ServerResponse) {
  const raw = String(body.text ?? "").trim()
  if (!raw) { res.writeHead(400); res.end("text required"); return }

  const { send, done } = sse(res)

  const agent = makeRuntime("capture")
  agent.register(searchArchive, fetchAndClip, exportTool)

  const goal = `Organize this note for the FlashNote knowledge base.
Use the classify_and_tag skill, then call search_archive to find related notes.
Output ONLY a JSON object (no extra text):

{
  "type": "idea|article|task|reference|insight|research",
  "tags": ["#tag1", "#tag2"],
  "summary": "one-line summary ≤80 chars",
  "connections": []
}

Note:
${raw}`

  const text = await streamAgent(agent, goal, [], send)

  const note = parseNoteOutput(text, raw, "personal")
  if (note) {
    await saveNote(note)
    send({ type: "saved", note })
  } else {
    const fallback = {
      id: generateId(), type: "idea" as const, tags: ["#unprocessed"],
      summary: raw.slice(0, 80), connections: [], source: "personal" as const,
      raw, qualityScore: 0.3, createdAt: Date.now(),
    }
    await saveNote(fallback)
    send({ type: "saved", note: fallback })
  }
  done()
}

async function handleResearch(body: Record<string, unknown>, res: ServerResponse) {
  const topic = String(body.topic ?? "").trim()
  if (!topic) { res.writeHead(400); res.end("topic required"); return }

  const { send, done } = sse(res)

  const agent = makeRuntime("research")
  agent.register(webSearch, fetchAndClip, searchArchive, exportTool)

  const goal = `Research this topic: "${topic}"

1. Use outline_research skill to plan
2. Call web_search with 2–3 queries
3. Call fetch_and_clip on promising URLs
4. Use summarize_source skill after each fetch
5. Write a report: TL;DR / key findings (with URLs) / comparison / conclusion / references`

  const text = await streamAgent(agent, goal, REPORT_CRITERIA, send)

  const note = {
    id: generateId(), type: "research" as const,
    tags: ["#research", `#${topic.replace(/\s+/g, "-").toLowerCase().slice(0, 30)}`],
    summary: `Research: ${topic.slice(0, 60)}`, connections: [],
    source: "personal" as const, raw: text,
    qualityScore: 0.85, createdAt: Date.now(),
  }
  await saveNote(note)
  send({ type: "saved", note })
  done()
}

async function handleExport(params: URLSearchParams, res: ServerResponse) {
  const format = params.get("format") ?? "digest"
  const agent = makeRuntime("capture")
  agent.register(exportTool)
  const result = await agent.run(`Call the export tool with format="${format}".`)
  res.writeHead(200, { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" })
  res.end(JSON.stringify({ result }))
}

async function handleExportDataset(params: URLSearchParams, res: ServerResponse) {
  const format = params.get("format") ?? "jsonl"
  const minQ = parseFloat(params.get("min_quality") ?? "0.7")
  const anon = params.get("anonymize") === "true"
  const agent = makeRuntime("capture")
  agent.register(exportDataset)
  const result = await agent.run(
    `Call export_dataset with format="${format}", min_quality=${minQ}, anonymize=${anon}.`
  )
  res.writeHead(200, { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" })
  res.end(JSON.stringify({ result }))
}

// ─── router ──────────────────────────────────────────────────────────────────

const server = createServer(async (req, res) => {
  cors(res)
  if (req.method === "OPTIONS") { res.writeHead(204); res.end(); return }

  const url = new URL(req.url ?? "/", `http://localhost:${PORT}`)
  const path = url.pathname
  const method = req.method ?? "GET"

  try {
    if (method === "GET" && (path === "/" || path === "/index.html")) {
      const html = await readFile(resolve(ROOT, "public/index.html"), "utf8")
      res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" })
      res.end(html); return
    }

    if (method === "GET" && path === "/api/notes") {
      const notes = await loadNotes(50)
      res.writeHead(200, { "Content-Type": "application/json" })
      res.end(JSON.stringify(notes)); return
    }

    if (method === "POST" && path === "/api/capture") {
      const body = await readBody(req)
      await handleCapture(body, res); return
    }

    if (method === "POST" && path === "/api/research") {
      const body = await readBody(req)
      await handleResearch(body, res); return
    }

    if (method === "GET" && path === "/api/export") {
      await handleExport(url.searchParams, res); return
    }

    if (method === "GET" && path === "/api/export-dataset") {
      await handleExportDataset(url.searchParams, res); return
    }

    res.writeHead(404); res.end("Not found")
  } catch (e) {
    console.error(e)
    if (!res.headersSent) { res.writeHead(500); res.end(String(e)) }
  }
})

server.listen(PORT, () => {
  console.log(`\x1b[1m\x1b[35mFlashNote\x1b[0m running at \x1b[36mhttp://localhost:${PORT}\x1b[0m`)
  if (!process.env.OPENAI_API_KEY) {
    console.warn("\x1b[33m⚠ OPENAI_API_KEY not set\x1b[0m")
  }
})
