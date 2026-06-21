import { HarnessLoop } from "@deepstrike/sdk/harness"
import type { LLMProvider } from "@deepstrike/sdk"
import type { FlashNoteRuntime } from "../runtime.js"

export function makeReportJudge(runtime: FlashNoteRuntime, evalProvider: LLMProvider): HarnessLoop {
  return new HarnessLoop(runtime.runner, evalProvider, { maxAttempts: 2 })
}

export const REPORT_CRITERIA = [
  "Report must cite at least 3 independent sources with clickable URLs",
  "Every major claim must have a supporting URL or note reference",
  "Word count must be between 600 and 1200 words",
  "Structure must include: TL;DR section, comparison table or bullet list, conclusion, and references",
  "No fabricated URLs — only URLs returned by web_search or fetch_and_clip",
]
