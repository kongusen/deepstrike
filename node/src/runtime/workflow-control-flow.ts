//! A#2: SDK-side execution of the kernel's control-flow workflow node kinds (Loop / Classify /
//! Tournament). The kernel owns the scheduling — it re-arms loops, prunes classify branches, and
//! runs the tournament bracket — and tells the SDK *which* kind a spawn is via the spawn descriptor
//! (`loop_max_iters` / `classify_labels` / `judge_match`). This module is the SDK half of the "one
//! agent per node + one additive result field" contract: it builds the prompt that solicits the
//! decision from the node's agent and extracts the matching result signal (`loopContinue` /
//! `classifyBranch` / `tournamentWinner`) the kernel reads back.

import { extractJsonValue } from "./output-schema.js"

/** Instruction appended to a loop node's goal: do the next increment, and signal when done. */
export function loopInstruction(maxIters: number): string {
  return (
    `This task runs as a LOOP (up to ${maxIters} iterations total). Do the next increment of work now. ` +
    `When you judge the overall task COMPLETE and no further iterations are needed, end your response ` +
    `with a JSON object {"loop_continue": false}. To request another iteration, omit it or return ` +
    `{"loop_continue": true}.`
  )
}

/** Instruction appended to a classify node's goal: pick exactly one of the kernel's branch labels. */
export function classifyInstruction(labels: string[]): string {
  return (
    `Classify the input and choose EXACTLY ONE label from: ${labels.map(l => JSON.stringify(l)).join(", ")}. ` +
    `Respond with ONLY a JSON object: {"branch": "<one of the labels>"}.`
  )
}

/** Build a tournament judge's goal: the controller's criterion + the two candidates to compare. */
export function judgeGoal(criterion: string, leftOutput: string, rightOutput: string): string {
  return (
    `${criterion}\n\nCompare the two candidate outputs below and decide which one better satisfies the ` +
    `criterion above.\n\n[CANDIDATE left]\n${leftOutput}\n\n[CANDIDATE right]\n${rightOutput}\n\n` +
    `Respond with ONLY a JSON object: {"winner": "left"} or {"winner": "right"}.`
  )
}

/** Extract a loop stop signal from a loop iteration's output. Returns the `loopContinue` value, or
 *  `undefined` when the agent gave no clear signal (⇒ the kernel runs the loop to `max_iters`).
 *  Accepts `{loop_continue: bool}` or, leniently, `{done: bool}` (continue = !done). */
export function extractLoopContinue(text: string): boolean | undefined {
  const v = extractJsonValue(text)
  if (v && typeof v === "object" && !Array.isArray(v)) {
    const o = v as Record<string, unknown>
    if (typeof o.loop_continue === "boolean") return o.loop_continue
    if (typeof o.loopContinue === "boolean") return o.loopContinue
    if (typeof o.done === "boolean") return !o.done
  }
  return undefined
}

/** Extract the chosen branch label from a classifier's output. Prefers `{branch: "..."}`; falls back
 *  to a bare label string that exactly matches one of the valid labels. Returns `undefined` when no
 *  recognizable choice was made (the kernel then prunes every branch — a safe "none matched"). */
export function extractClassifyBranch(text: string, labels: string[]): string | undefined {
  const v = extractJsonValue(text)
  if (v && typeof v === "object" && !Array.isArray(v)) {
    const o = v as Record<string, unknown>
    if (typeof o.branch === "string") return o.branch
    if (typeof o.label === "string") return o.label
  }
  if (typeof v === "string" && labels.includes(v)) return v
  const trimmed = (text ?? "").trim()
  if (labels.includes(trimmed)) return trimmed
  return undefined
}

/** Extract a tournament judge's verdict ("left" or "right"). Defaults to "left" when the verdict is
 *  unparseable, so the bracket always advances to a champion rather than stalling with no winner. */
export function extractJudgeWinner(text: string): "left" | "right" {
  const v = extractJsonValue(text)
  if (v && typeof v === "object" && !Array.isArray(v)) {
    const w = (v as Record<string, unknown>).winner
    if (w === "right") return "right"
    if (w === "left") return "left"
  }
  const lowered = (text ?? "").toLowerCase()
  // Last resort: a bare mention. Bias to "left" on ambiguity (both/neither mentioned).
  if (lowered.includes("right") && !lowered.includes("left")) return "right"
  return "left"
}
