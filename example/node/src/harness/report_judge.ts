import { HarnessLoop } from "@deepstrike/sdk"
import type { Agent, LLMProvider } from "@deepstrike/sdk"

export function makeReportJudge(agent: Agent, evalProvider: LLMProvider): HarnessLoop {
  return new HarnessLoop(agent, evalProvider, { maxAttempts: 2 })
}

export const REPORT_CRITERIA = [
  "Report must cite at least 3 independent sources with clickable URLs",
  "Every major claim must have a supporting URL or note reference",
  "Word count must be between 600 and 1200 words",
  "Structure must include: TL;DR section, comparison table or bullet list, conclusion, and references",
  "No fabricated URLs — only URLs returned by web_search or fetch_and_clip",
]
