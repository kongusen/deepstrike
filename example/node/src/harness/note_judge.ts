import { EvalLoopHarness } from "@deepstrike/sdk"
import type { QualityGate, HarnessRequest, HarnessOutcome } from "@deepstrike/sdk"
import type { FlashNoteRuntime } from "../runtime.js"

const noteGate: QualityGate = {
  async evaluate(_req: HarnessRequest, outcome: HarnessOutcome): Promise<boolean> {
    const text = outcome.result
    const match = text.match(/```json\s*([\s\S]*?)```/) ?? text.match(/\{[\s\S]*"type"[\s\S]*\}/)
    if (!match) return false
    try {
      const parsed = JSON.parse(match[1] ?? match[0]) as {
        type?: string; tags?: unknown[]; summary?: string
      }
      const validType = ["idea", "article", "task", "reference", "insight", "research"].includes(parsed.type ?? "")
      const tagsOk = Array.isArray(parsed.tags) && parsed.tags.length >= 2
      const summaryOk = typeof parsed.summary === "string" && parsed.summary.length > 0 && parsed.summary.length <= 80
      return validType && tagsOk && summaryOk
    } catch {
      return false
    }
  },
}

export function makeNoteJudge(runtime: FlashNoteRuntime): EvalLoopHarness {
  return new EvalLoopHarness(runtime.runner, noteGate, 2)
}

export const NOTE_CRITERIA = [
  "Output must be a valid JSON object",
  "Must have at least 2 tags in #tag format",
  "summary must be ≤80 characters",
  "type must be one of: idea, article, task, reference, insight, research",
]
