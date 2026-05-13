import { EvalLoopHarness } from "@deepstrike/sdk"
import type { QualityGate, HarnessRequest, HarnessOutcome } from "@deepstrike/sdk"
import type { Agent } from "@deepstrike/sdk"
import { loadNotes } from "../archive.js"

const contributionGate: QualityGate = {
  async evaluate(_req: HarnessRequest, outcome: HarnessOutcome): Promise<boolean> {
    const text = outcome.result
    const match = text.match(/```json\s*([\s\S]*?)```/) ?? text.match(/\{[\s\S]*"type"[\s\S]*\}/)
    if (!match) return false
    try {
      const parsed = JSON.parse(match[1] ?? match[0]) as {
        type?: string; tags?: unknown[]; summary?: string; body?: string
      }
      const tagsOk = Array.isArray(parsed.tags) && parsed.tags.length >= 2
      const s = parsed.summary ?? ""
      const summaryOk = typeof s === "string" && s.length > 10 && s.length <= 80
      // Body must have substance: at least 80 chars with specific content
      const bodyOk = typeof parsed.body === "string" && parsed.body.length >= 80

      if (!tagsOk || !summaryOk || !bodyOk) return false

      // Soft dedup: reject if summary is too similar to existing notes
      const notes = await loadNotes()
      const newSummary = s.toLowerCase()
      const tooSimilar = notes.some(n => {
        const existing = n.summary.toLowerCase()
        const overlap = newSummary.split(" ").filter(w => existing.includes(w)).length
        return overlap / newSummary.split(" ").length > 0.85
      })
      return !tooSimilar
    } catch {
      return false
    }
  },
}

export function makeContributionJudge(agent: Agent): EvalLoopHarness {
  return new EvalLoopHarness(agent, contributionGate, 2)
}

export const CONTRIBUTION_CRITERIA = [
  "Output must be a valid JSON object with type, tags, summary, and body fields",
  "summary must be 10–80 characters, specific and quotable — not generic",
  "body must be at least 80 characters with a concrete example, number, or story",
  "Must have at least 2 tags; first tag should reflect the domain",
  "Content must be unique — not a rephrasing of something already in the archive",
]
