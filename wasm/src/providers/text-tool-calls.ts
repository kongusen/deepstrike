// Plan task 9.0 · tool calls a model wrote as text.
//
// DeepSeek sometimes answers with its native tool-call markup (DSML) in the message text instead of
// the API's `tool_calls` field, often in a degraded form:
//
//   <｜DSML｜invoke name="read_file">
//   <｜DSML｜parameter name="path" string="true">logs/svc-13.log</｜DSML｜parameter>
//   </｜DSML｜invoke>
//
// (or `｜｜DSML｜｜ parameter` with doubled bars and spaces). Left as text it enters history, the model
// imitates it on later turns, and the run loops until its turn limit. The dialect layer therefore
// keeps the markup out of the text: it parses it into real tool calls, or — when it cannot — fails
// the call as a retryable protocol error so the kernel's recovery asks again.

export interface TextToolCall {
  name: string
  arguments: Record<string, unknown>
}

const MARKUP_START = /<\s*\/?\s*[｜|]+\s*DSML/i
const TAG = /<\s*(\/?)\s*[｜|]+\s*DSML\s*[｜|]+\s*([A-Za-z_]+)([^>]*)>/gi
/** A text tail that may be the start of a marker split across stream chunks. */
const PARTIAL_START = /<\s*\/?\s*(?:[｜|]+\s*(?:D(?:S(?:M)?)?)?)?$/i
const PARTIAL_LOOKBACK = 24

/** Index of the first DSML marker in `text`, or -1. */
export function markupStart(text: string): number {
  const match = MARKUP_START.exec(text)
  return match ? match.index : -1
}

/** How many trailing characters of `text` could begin a marker and must wait for the next chunk. */
export function partialMarkupLength(text: string): number {
  const tail = text.slice(-PARTIAL_LOOKBACK)
  const at = tail.lastIndexOf("<")
  if (at < 0) return 0
  return PARTIAL_START.test(tail.slice(at)) ? tail.length - at : 0
}

function attribute(attrs: string, name: string): string | undefined {
  const match = new RegExp(`\\b${name}\\s*=\\s*"([^"]*)"`, "i").exec(attrs)
  return match?.[1]
}

function parameterValue(raw: string, isString: boolean): unknown {
  if (isString) return raw
  try { return JSON.parse(raw) } catch { return raw }
}

/** The tool calls in `markup`, or undefined when it is not a complete, well-formed set of calls. */
export function parseTextToolCalls(markup: string): TextToolCall[] | undefined {
  const calls: TextToolCall[] = []
  let current: TextToolCall | undefined
  let parameter: { name: string; isString: boolean; start: number } | undefined
  for (const match of markup.matchAll(TAG)) {
    const [whole, close, rawTag, attrs] = match
    const tag = rawTag.toLowerCase()
    const at = match.index ?? 0
    if (tag === "invoke") {
      if (!close) {
        const name = attribute(attrs, "name")
        if (!name || current || parameter) return undefined
        current = { name, arguments: {} }
      } else {
        if (!current || parameter) return undefined
        calls.push(current)
        current = undefined
      }
    } else if (tag === "parameter") {
      if (!close) {
        const name = attribute(attrs, "name")
        if (!name || !current || parameter) return undefined
        parameter = { name, isString: attribute(attrs, "string") !== "false", start: at + whole.length }
      } else {
        if (!current || !parameter) return undefined
        current.arguments[parameter.name] = parameterValue(markup.slice(parameter.start, at), parameter.isString)
        parameter = undefined
      }
    }
    // Wrappers (`function_calls`, `tool_calls`, a bare `calls`) carry nothing.
  }
  if (current || parameter || calls.length === 0) return undefined
  return calls
}

/** A call id for a parsed call: unique, so read-backs and tool results never collide. */
export function textToolCallId(): string {
  return `call_text_${Math.random().toString(36).slice(2, 12)}`
}

/** Markup that does not parse fails like the Anthropic adapter's textual-tool-call guard (SPC-022):
 *  same provider code and message, retryable, and the body is never echoed. */
export const TEXTUAL_TOOL_CALL_CODE = "textual_tool_call"
export const TEXTUAL_TOOL_CALL_MESSAGE = "Provider emitted a tool call as text instead of a native tool block"
