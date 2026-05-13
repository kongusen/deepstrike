import { Agent } from "@deepstrike/sdk"
import type { AgentOptions } from "@deepstrike/sdk"
import { makeProvider } from "./provider.js"
import { makePolicy } from "./governance/policy.js"
import { makeArchiveSource } from "./knowledge/archive_source.js"
import { makeFileDreamStore } from "./memory/dream_store.js"
import { SKILLS_DIR } from "./paths.js"

export type AgentMode = "capture" | "research" | "interview"

export function makeAgent(mode: AgentMode = "capture", overrides: Partial<AgentOptions> = {}) {
  const dreamStore = makeFileDreamStore()

  return new Agent(makeProvider(), {
    maxTokens: 4096,
    maxTurns: mode === "research" ? 20 : 5,
    skillDir: SKILLS_DIR,
    knowledgeSource: makeArchiveSource(),
    governance: makePolicy(),
    dreamStore,
    agentId: "flashnote",
    ...overrides,
  })
}
