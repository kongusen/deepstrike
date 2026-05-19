import { FileSessionLog, LocalExecutionPlane, RuntimeRunner, collectText } from "@deepstrike/sdk"
import type { RegisteredTool, RuntimeOptions, StreamEvent } from "@deepstrike/sdk"
import { randomUUID } from "node:crypto"
import { makeProvider } from "./provider.js"
import { makePolicy } from "./governance/policy.js"
import { makeArchiveSource } from "./knowledge/archive_source.js"
import { makeFileDreamStore } from "./memory/dream_store.js"
import { OUTPUT_DIR, SKILLS_DIR } from "./paths.js"

export type RuntimeMode = "capture" | "research" | "interview"
export type RuntimeOverride = Partial<Omit<RuntimeOptions, "provider" | "sessionLog" | "executionPlane">>

export class FlashNoteRuntime {
  constructor(
    readonly runner: RuntimeRunner,
    private readonly plane: LocalExecutionPlane,
  ) {}

  register(...tools: RegisteredTool[]): this {
    this.plane.register(...tools)
    return this
  }

  run(goal: string, criteria: string[] = [], extensions?: Record<string, unknown>, sessionId = randomUUID()): Promise<string> {
    return collectText(this.runner.run({ sessionId, goal, criteria, extensions }))
  }

  runStreaming(
    goal: string,
    criteria: string[] = [],
    extensions?: Record<string, unknown>,
    sessionId = randomUUID(),
  ): AsyncIterable<StreamEvent> {
    return this.runner.run({ sessionId, goal, criteria, extensions })
  }

  dream(agentId: string, nowMs = Date.now()) {
    return this.runner.dream(agentId, nowMs)
  }

  wake(sessionId: string, extensions?: Record<string, unknown>) {
    return this.runner.wake(sessionId, extensions)
  }
}

export function makeRuntime(mode: RuntimeMode = "capture", overrides: RuntimeOverride = {}) {
  const dreamStore = makeFileDreamStore()
  const plane = new LocalExecutionPlane()
  const runner = new RuntimeRunner({
    provider: makeProvider(),
    sessionLog: new FileSessionLog(`${OUTPUT_DIR}/sessions`),
    executionPlane: plane,
    maxTokens: 4096,
    maxTurns: mode === "research" ? 20 : 5,
    skillDir: SKILLS_DIR,
    knowledgeSource: makeArchiveSource(),
    governance: makePolicy(),
    dreamStore,
    agentId: "flashnote",
    ...overrides,
  })
  return new FlashNoteRuntime(runner, plane)
}
