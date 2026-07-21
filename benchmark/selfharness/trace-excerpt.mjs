/**
 * renderExcerpt — paper Fig-7 style compact, deterministic, bounded trajectory rendering.
 *
 * Turns a bench `*.events.json` stream (`{seq, event}[]`) into a small, byte-stable text block a
 * model (miner / proposer) can read as evidence for one failure. It is pure: no clock, no random,
 * no Map iteration-order dependence — the same events always render the same bytes. This is the
 * evidence-anchoring half of the Self-Harness weakness-mining stage: the LLM sees a faithful
 * excerpt, never the raw log.
 *
 * Rendered lines (in event-stream order):
 *   - llm_completed  → `T{turn} S: {content ≤120ch}` (tool names when content is empty)
 *   - tool_completed → one line per result:
 *       `T{turn} tool {name}({args ≤80ch}) -> ok`   or   `... -> ERROR[{error_kind}]`
 *       (name/args resolved from the matching tool_requested call by call_id)
 *   - tool_denied    → `T{turn} DENIED {tool_name}: {reason ≤80ch}`
 *   - final line     → `END {run_terminal.reason ?? "unknown"}`
 *
 * When the joined text exceeds `maxChars`, the middle is dropped and replaced by a single
 * `…[{n} steps omitted]…` marker, keeping the head and the tail (the terminal state). The result
 * is guaranteed `≤ maxChars`.
 *
 * @typedef {Object} EventEnvelope
 * @property {number} [seq]
 * @property {Record<string, any>} event
 */

const SEP = "\n"

/** Unwrap `{seq, event}[]` (bench dump) or a bare event array into inner event objects. */
function eventsOf(stream) {
  if (!Array.isArray(stream)) return []
  return stream.map(e => (e && typeof e === "object" && "event" in e ? e.event : e)).filter(Boolean)
}

/** Collapse all whitespace runs to single spaces and trim — keeps every rendered line single-line. */
function collapse(s) {
  return String(s ?? "").replace(/\s+/g, " ").trim()
}

/** Collapse then hard-cap at `n` chars, marking truncation with a trailing ellipsis (total ≤ n). */
function clip(s, n) {
  const str = collapse(s)
  return str.length <= n ? str : str.slice(0, Math.max(0, n - 1)) + "…"
}

/** Joined length of an array of lines when joined by SEP. */
function joinLen(lines) {
  let total = 0
  for (const l of lines) total += l.length
  return total + Math.max(0, lines.length - 1)
}

/**
 * Render a compact, bounded excerpt of one session's event stream.
 * @param {EventEnvelope[] | Record<string, any>[]} stream  Bench `*.events.json` shape (or bare events).
 * @param {{ maxChars?: number }} [opts]
 * @returns {string}
 */
export function renderExcerpt(stream, opts = {}) {
  const maxChars = opts.maxChars ?? 4000
  const events = eventsOf(stream)

  // Resolve call_id → {name, arguments} from tool_requested (what the kernel let through),
  // falling back to llm_completed.tool_calls when a request event is absent.
  /** @type {Record<string, { name: string, arguments: string }>} */
  const calls = {}
  const recordCall = c => {
    if (c && typeof c === "object" && typeof c.id === "string") {
      calls[c.id] = { name: c.name ?? "unknown", arguments: c.arguments ?? "" }
    }
  }
  for (const ev of events) {
    if (ev.kind === "llm_completed" && Array.isArray(ev.tool_calls)) ev.tool_calls.forEach(recordCall)
    if (ev.kind === "tool_requested" && Array.isArray(ev.calls)) ev.calls.forEach(recordCall)
  }

  let termination = "unknown"
  for (const ev of events) if (ev.kind === "run_terminal") termination = String(ev.reason ?? "unknown")

  /** @type {string[]} */
  const lines = []
  for (const ev of events) {
    if (ev.kind === "llm_completed") {
      const content = collapse(ev.content)
      if (content) {
        lines.push(`T${ev.turn} S: ${clip(content, 120)}`)
      } else if (Array.isArray(ev.tool_calls) && ev.tool_calls.length > 0) {
        lines.push(`T${ev.turn} S: ${ev.tool_calls.map(c => c.name ?? "unknown").join(", ")}`)
      } else {
        lines.push(`T${ev.turn} S:`)
      }
    } else if (ev.kind === "tool_completed" && Array.isArray(ev.results)) {
      for (const r of ev.results) {
        const meta = calls[r.call_id] ?? { name: "unknown", arguments: "" }
        const status = r.is_error ? `ERROR[${r.error_kind ?? "unknown"}]` : "ok"
        lines.push(`T${ev.turn} tool ${meta.name}(${clip(meta.arguments, 80)}) -> ${status}`)
      }
    } else if (ev.kind === "tool_denied") {
      lines.push(`T${ev.turn} DENIED ${ev.tool_name}: ${clip(ev.reason, 80)}`)
    }
  }
  lines.push(`END ${termination}`)

  const full = lines.join(SEP)
  if (full.length <= maxChars) return full

  // Deterministic middle-drop: keep a head budget and a tail budget, reserving room for the marker.
  const RESERVE = 64
  const half = Math.max(0, Math.floor((maxChars - RESERVE) / 2))

  /** @type {string[]} */
  const head = []
  for (const line of lines) {
    if (joinLen([...head, line]) > half) break
    head.push(line)
  }
  /** @type {string[]} */
  let tail = []
  const tailBudget = Math.max(0, maxChars - RESERVE - joinLen(head))
  for (let i = lines.length - 1; i >= head.length; i--) {
    if (joinLen([lines[i], ...tail]) > tailBudget) break
    tail = [lines[i], ...tail]
  }

  const compose = (h, t) => {
    const n = lines.length - h.length - t.length
    const marker = `…[${n} steps omitted]…`
    return { n, out: [...h, marker, ...t].join(SEP) }
  }

  let { n, out } = compose(head, tail)
  // Safety: guarantee the ≤ maxChars bound even if the marker pushed us over. Trim head first
  // (the tail carries END and the most recent, highest-signal steps).
  while (out.length > maxChars && head.length > 0) {
    head.pop()
    ;({ n, out } = compose(head, tail))
  }
  while (out.length > maxChars && tail.length > 1) {
    tail = tail.slice(1)
    ;({ n, out } = compose(head, tail))
  }
  return out
}
