/**
 * Self-Harness H1.2 — declarative event→note rules (the runtime control-policy surface).
 *
 * A `NudgeRule` says "when this session event fires, push this note to the model". It generalizes the
 * two hard-coded precedents (EntropyWatch.notify_model, the RepeatFuse STOP text) into data the
 * self-harness loop can rewrite. `NudgeEngine` is a pure, clock-free class: `observe(event)` folds one
 * session event into its per-rule state and returns the notes that fired, which the runner lowers to
 * the `injectNote` signal channel. No I/O, no randomness — so the trigger matrix is exhaustively
 * unit-testable and two engines never share state.
 */
import type { RuntimeSignalUrgency } from "../signals/types.js"
import type { SessionEvent } from "../runtime/session-log.js"

export type NudgeTrigger =
  | { kind: "tool_error"; errorKind?: string; toolName?: string } // an is_error result in tool_completed
  | { kind: "tool_denied"; reasonIncludes?: string }              // includes repeat-fuse denies
  | { kind: "tool_calls_at_least"; count: number }                // cumulative tool_requested count first reaches
  | { kind: "turns_at_least"; count: number }                     // turn first reaches
  | { kind: "entropy_alert" }                                     // rides the EntropyWatch alert event

export interface NudgeRule {
  id: string
  on: NudgeTrigger
  /** Template — supports {{tool_name}} {{error_kind}} {{turn}} only. */
  note: string
  /** Default "normal" (the injectNote default). */
  urgency?: RuntimeSignalUrgency
  /** Turns that must pass after a fire before the rule may fire again. Default 3. */
  cooldownTurns?: number
  /** Total fires allowed for the whole run. Default 2. */
  maxFires?: number
}

const MAX_RULES = 16
const MAX_NOTE_CHARS = 2000
const DEFAULT_COOLDOWN_TURNS = 3
const DEFAULT_MAX_FIRES = 2
const ALLOWED_TEMPLATE_VARS: readonly string[] = ["tool_name", "error_kind", "turn"]
const URGENCIES: readonly RuntimeSignalUrgency[] = ["low", "normal", "high", "critical"]

// ── Load validation ──────────────────────────────────────────────────────────

function validateTrigger(on: NudgeTrigger, id: string): void {
  if (typeof on !== "object" || on === null) throw new TypeError(`nudge rule ${id}: trigger must be an object`)
  switch (on.kind) {
    case "tool_error":
      if (on.errorKind !== undefined && typeof on.errorKind !== "string") {
        throw new TypeError(`nudge rule ${id}: tool_error.errorKind must be a string`)
      }
      if (on.toolName !== undefined && typeof on.toolName !== "string") {
        throw new TypeError(`nudge rule ${id}: tool_error.toolName must be a string`)
      }
      return
    case "tool_denied":
      if (on.reasonIncludes !== undefined && typeof on.reasonIncludes !== "string") {
        throw new TypeError(`nudge rule ${id}: tool_denied.reasonIncludes must be a string`)
      }
      return
    case "tool_calls_at_least":
    case "turns_at_least":
      if (!Number.isInteger(on.count) || on.count < 1) {
        throw new RangeError(`nudge rule ${id}: ${on.kind}.count must be a positive integer`)
      }
      return
    case "entropy_alert":
      return
    default:
      throw new RangeError(`nudge rule ${id}: unknown trigger kind: ${(on as { kind: string }).kind}`)
  }
}

function validateTemplateVars(note: string, id: string): void {
  const re = /\{\{([^{}]*)\}\}/g
  let match: RegExpExecArray | null
  while ((match = re.exec(note)) !== null) {
    if (!ALLOWED_TEMPLATE_VARS.includes(match[1])) {
      throw new RangeError(`nudge rule ${id}: unsupported template variable {{${match[1]}}}`)
    }
  }
}

/** Structural load check — throws on a malformed / oversized / duplicate-id rule set. */
export function validateNudgeRules(rules: NudgeRule[]): void {
  if (!Array.isArray(rules)) throw new TypeError("nudges must be an array")
  if (rules.length > MAX_RULES) throw new RangeError(`at most ${MAX_RULES} nudge rules (got ${rules.length})`)
  const ids = new Set<string>()
  for (const rule of rules) {
    if (typeof rule !== "object" || rule === null) throw new TypeError("each nudge rule must be an object")
    if (typeof rule.id !== "string" || rule.id.length === 0) {
      throw new TypeError("nudge rule id must be a non-empty string")
    }
    if (ids.has(rule.id)) throw new RangeError(`duplicate nudge rule id: ${rule.id}`)
    ids.add(rule.id)
    validateTrigger(rule.on, rule.id)
    if (typeof rule.note !== "string" || rule.note.length === 0) {
      throw new TypeError(`nudge rule ${rule.id}: note must be a non-empty string`)
    }
    if (rule.note.length > MAX_NOTE_CHARS) {
      throw new RangeError(`nudge rule ${rule.id}: note exceeds ${MAX_NOTE_CHARS} chars`)
    }
    validateTemplateVars(rule.note, rule.id)
    if (rule.cooldownTurns !== undefined && (!Number.isInteger(rule.cooldownTurns) || rule.cooldownTurns < 0)) {
      throw new RangeError(`nudge rule ${rule.id}: cooldownTurns must be a non-negative integer`)
    }
    if (rule.maxFires !== undefined && (!Number.isInteger(rule.maxFires) || rule.maxFires < 1)) {
      throw new RangeError(`nudge rule ${rule.id}: maxFires must be a positive integer`)
    }
    if (rule.urgency !== undefined && !URGENCIES.includes(rule.urgency)) {
      throw new RangeError(`nudge rule ${rule.id}: invalid urgency ${rule.urgency}`)
    }
  }
}

// ── Engine ───────────────────────────────────────────────────────────────────

export interface NudgeOutput {
  note: string
  urgency: RuntimeSignalUrgency
}

interface TemplateContext {
  tool_name?: string
  error_kind?: string
  turn: number
}

interface RuleState {
  fires: number
  lastFireTurn: number
}

function renderNote(template: string, ctx: TemplateContext): string {
  return template
    .replace(/\{\{tool_name\}\}/g, ctx.tool_name ?? "")
    .replace(/\{\{error_kind\}\}/g, ctx.error_kind ?? "")
    .replace(/\{\{turn\}\}/g, String(ctx.turn))
}

export class NudgeEngine {
  private readonly rules: NudgeRule[]
  private readonly state: RuleState[]
  /** Cumulative tool_requested calls, for `tool_calls_at_least` edge detection. */
  private toolCallsSeen = 0
  /** Highest turn observed, for cooldown + `turns_at_least` edge detection. */
  private currentTurn = 0
  /** call_id → tool name, so a tool_completed error can name its tool (results carry no name). */
  private readonly callNames = new Map<string, string>()

  constructor(rules: NudgeRule[]) {
    validateNudgeRules(rules)
    // Defensive shallow copy: the caller can mutate its array afterwards without touching the engine.
    this.rules = rules.map(rule => ({ ...rule }))
    this.state = this.rules.map(() => ({ fires: 0, lastFireTurn: 0 }))
  }

  /** Fold one session event into rule state; return the notes that fired this event, in rule order. */
  observe(event: SessionEvent): NudgeOutput[] {
    const prevToolCalls = this.toolCallsSeen
    const prevTurn = this.currentTurn

    if (event.kind === "tool_requested") {
      for (const call of event.calls) this.callNames.set(call.id, call.name)
      this.toolCallsSeen += event.calls.length
    }
    if ("turn" in event && typeof event.turn === "number") {
      this.currentTurn = Math.max(this.currentTurn, event.turn)
    }

    const out: NudgeOutput[] = []
    for (let i = 0; i < this.rules.length; i++) {
      const rule = this.rules[i]
      const matched = this.match(rule.on, event, prevToolCalls, prevTurn)
      if (!matched) continue
      const st = this.state[i]
      if (st.fires >= (rule.maxFires ?? DEFAULT_MAX_FIRES)) continue
      if (st.fires > 0 && this.currentTurn - st.lastFireTurn < (rule.cooldownTurns ?? DEFAULT_COOLDOWN_TURNS)) continue
      st.fires += 1
      st.lastFireTurn = this.currentTurn
      out.push({ note: renderNote(rule.note, matched), urgency: rule.urgency ?? "normal" })
    }
    return out
  }

  /** Returns the template context when the trigger matches this event, else null. */
  private match(
    on: NudgeTrigger,
    event: SessionEvent,
    prevToolCalls: number,
    prevTurn: number,
  ): TemplateContext | null {
    switch (on.kind) {
      case "tool_error": {
        if (event.kind !== "tool_completed") return null
        for (const result of event.results) {
          if (!result.is_error) continue
          if (on.errorKind !== undefined && result.error_kind !== on.errorKind) continue
          const name = this.callNames.get(result.call_id)
          if (on.toolName !== undefined && name !== on.toolName) continue
          return { tool_name: name ?? "", error_kind: result.error_kind ?? "", turn: event.turn }
        }
        return null
      }
      case "tool_denied": {
        if (event.kind !== "tool_denied") return null
        if (on.reasonIncludes !== undefined && !event.reason.includes(on.reasonIncludes)) return null
        return { tool_name: event.tool_name, error_kind: "", turn: event.turn }
      }
      case "tool_calls_at_least": {
        // Edge: the cumulative count crosses the threshold on THIS event (monotonic ⇒ fires once).
        if (prevToolCalls < on.count && this.toolCallsSeen >= on.count) {
          return { turn: this.currentTurn }
        }
        return null
      }
      case "turns_at_least": {
        if (prevTurn < on.count && this.currentTurn >= on.count) {
          return { turn: this.currentTurn }
        }
        return null
      }
      case "entropy_alert": {
        if (event.kind !== "entropy_alert") return null
        return { turn: event.turn }
      }
    }
  }
}
