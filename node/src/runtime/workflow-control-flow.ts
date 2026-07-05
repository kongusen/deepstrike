//! A#2: SDK-side execution of the kernel's control-flow workflow node kinds (Loop / Classify /
//! Tournament). The kernel owns the scheduling — it re-arms loops, prunes classify branches, and
//! runs the tournament bracket — and tells the SDK *which* kind a spawn is via the spawn descriptor
//! (`loop_max_iters` / `classify_labels` / `judge_match`). This module is the SDK half of the "one
//! agent per node + one additive result field" contract: it builds the prompt that solicits the
//! decision from the node's agent and extracts the matching result signal (`loopContinue` /
//! `classifyBranch` / `tournamentWinner`) the kernel reads back.

import { extractJsonValue } from "./output-schema.js"

/** Instruction appended to a loop iteration's goal. DW-3: the continuation verb is the
 *  kernel-adjudicated `pace` meta-tool (armed on every iteration run), not a text blob — one
 *  vocabulary shared with the round-level loop agent. Iterations share one session, so "the work
 *  so far" is simply the visible transcript. */
export function loopInstruction(maxIters: number, iteration = 0): string {
  return (
    `This task runs as a LOOP — this is iteration ${iteration + 1} of up to ${maxIters}. Your prior ` +
    `iterations' work (if any) is visible above; do the NEXT increment now. Then call the \`pace\` tool: ` +
    `\`{"next": "continue"}\` to request another iteration, or \`{"next": "stop"}\` when the overall ` +
    `task is complete. Ending without calling \`pace\` also completes the loop.`
  )
}

/** W-N2: dependency outputs appended to a dependent node's goal — a DAG edge carries data, not
 *  just ordering (fan-out→synthesize was an uninformed synthesis without this). Each dependency's
 *  output is clipped so a chain of large nodes can't blow the child's context; empty/unknown
 *  outputs are skipped. Returns "" when the node has no dependencies. */
export function dependencyOutputsNote(
  inputAgentIds: string[] | undefined,
  outputs: Map<string, string> | undefined,
  maxPerDep = 8_000,
): string {
  if (!inputAgentIds?.length || !outputs) return ""
  const blocks = inputAgentIds
    .map(id => {
      const out = outputs.get(id) ?? ""
      if (!out) return ""
      const clipped = out.length > maxPerDep ? `${out.slice(0, maxPerDep)}\n…[truncated]` : out
      return `[dependency ${id} output]\n${clipped}`
    })
    .filter(Boolean)
  return blocks.join("\n\n")
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
