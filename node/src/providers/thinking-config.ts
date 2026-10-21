/**
 * Provider-agnostic thinking intensity control.
 *
 * The latest reasoning models (Claude extended thinking, OpenAI o-series, DeepSeek R1,
 * Qwen QwQ) all expose a knob for *how hard to think* on a given turn. Each provider
 * names it differently — `budget_tokens`, `reasoning.effort`, `reasoning_effort`,
 * `thinking_budget` — but the semantic is the same: a dial from "quick answer" to
 * "think deeply."
 *
 * This module defines the canonical knob and per-provider projectors so the runner
 * can apply a per-turn thinking intensity without each adapter reinventing the wiring.
 */

/**
 * Normalised thinking intensity. The runner passes this as `extensions.thinkingIntensity`
 * on every `call_provider` action; each provider adapter projects it onto its native
 * wire parameter.
 *
 * - `"auto"`     — provider default (no override; whatever the model picks)
 * - `"minimal"`  — fastest, least reasoning (o-series "low", Claude budget floor)
 * - `"standard"` — balanced (o-series "medium", Claude moderate budget)
 * - `"deep"`     — maximum reasoning (o-series "high", Claude large budget)
 */
export type ThinkingIntensity = "auto" | "minimal" | "standard" | "deep"

export interface ThinkingIntensityConfig {
  /** The intensity level to apply. `"auto"` means "do not override; pass nothing." */
  intensity: ThinkingIntensity
}

/**
 * Extensions key the runner sets on every `call_provider` turn when the agent (or host)
 * has requested a thinking-intensity change. Provider adapters read this to project
 * onto their native wire parameter.
 */
export const THINKING_INTENSITY_EXTENSION_KEY = "thinkingIntensity"

// ---------------------------------------------------------------------------
// Per-provider projectors — turn a canonical intensity into native wire params.
// ---------------------------------------------------------------------------

/**
 * Anthropic: `thinking: { type: "enabled", budget_tokens: N }` + the
 * `interleaved-thinking-2025-05-14` beta header. Returns the wire additions
 * (merged into request params by the adapter) and any betas to append.
 */
export function projectAnthropicThinking(
  intensity: ThinkingIntensity,
): { params: Record<string, unknown>; betas: string[] } | undefined {
  if (intensity === "auto") return undefined
  // Anthropic requires budget_tokens ≥ 1024 and ≤ max_tokens.
  const budget = ANTHROPIC_BUDGET_TOKENS[intensity]
  return {
    params: { thinking: { type: "enabled", budget_tokens: budget } },
    betas: ["interleaved-thinking-2025-05-14"],
  }
}

const ANTHROPIC_BUDGET_TOKENS: Record<Exclude<ThinkingIntensity, "auto">, number> = {
  minimal: 1024,
  standard: 8000,
  deep: 32000,
}

/**
 * OpenAI Responses (o-series): `reasoning: { effort: "low" | "medium" | "high" }`.
 * Merged into the Responses API `params` object.
 */
export function projectOpenAIResponsesThinking(
  intensity: ThinkingIntensity,
): Record<string, unknown> | undefined {
  if (intensity === "auto") return undefined
  const effort = OPENAI_EFFORT[intensity]
  return { reasoning: { effort } }
}

/**
 * OpenAI Chat Completions / DeepSeek: `reasoning_effort: "high" | "max"` plus
 * `extra_body.thinking: { type: "enabled" | "disabled" }`. DeepSeek only supports
 * "high" and "max" effort — "minimal" maps to "high" (the lowest DeepSeek offers)
 * but we also set `thinking: { type: "disabled" }` to turn it off entirely.
 */
export function projectDeepSeekThinking(
  intensity: ThinkingIntensity,
): Record<string, unknown> | undefined {
  if (intensity === "auto") return undefined
  if (intensity === "minimal") {
    return {
      reasoning_effort: "high",
      extra_body: { thinking: { type: "disabled" } },
      __deepstrikeThinkingEnabled: false,
    }
  }
  return {
    reasoning_effort: intensity === "deep" ? "max" : "high",
    extra_body: { thinking: { type: "enabled" } },
    __deepstrikeThinkingEnabled: true,
  }
}

/**
 * Qwen (DashScope OpenAI-compatible): `extra_body.enable_thinking: true` and
 * `extra_body.thinking_budget: N` (token budget for reasoning).
 */
export function projectQwenThinking(
  intensity: ThinkingIntensity,
): Record<string, unknown> | undefined {
  if (intensity === "auto") return undefined
  if (intensity === "minimal") {
    return { extra_body: { enable_thinking: false } }
  }
  return {
    extra_body: { enable_thinking: true, thinking_budget: QWEN_BUDGET[intensity] },
  }
}

const QWEN_BUDGET: Record<Exclude<ThinkingIntensity, "auto" | "minimal">, number> = {
  standard: 4096,
  deep: 16384,
}

const OPENAI_EFFORT: Record<Exclude<ThinkingIntensity, "auto">, "low" | "medium" | "high"> = {
  minimal: "low",
  standard: "medium",
  deep: "high",
}
