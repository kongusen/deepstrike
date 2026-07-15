/**
 * v0.2.30 API-surface guard. Locks in the streamlined root surface and the subpath split so a stray
 * re-export can't silently re-bloat the public API. See .local-docs/specs/api-streamline-v0.2.300.md.
 */
import * as root from "../src/index.js"
import * as providers from "../src/providers/public.js"
import * as workflow from "../src/workflow/public.js"
import * as planes from "../src/planes/public.js"
import * as memory from "../src/memory/public.js"
import * as harness from "../src/harness/public.js"
import * as os from "../src/os/public.js"
import { OpenAIProvider } from "../src/index.js"

describe("root surface", () => {
  it("exposes the Tier-1 entry points", () => {
    for (const name of [
      "runAgent", "runFanout", "RuntimeRunner", "collectText",
      "LocalExecutionPlane", "InMemorySessionLog", "FileSessionLog",
      "tool", "streamingTool", "safeTool", "ok", "fail",
      "AnthropicProvider", "OpenAIProvider", "OpenAIResponsesProvider", "createProvider",
      "Governance", "AgentPool",
    ]) {
      expect(root).toHaveProperty(name)
    }
  })

  it("does NOT leak machinery that moved to subpaths or was internalized", () => {
    for (const name of [
      // moved to subpaths
      "OpenAIChatProvider", "DeepSeekProvider", "builtinReducers", "SubAgentOrchestrator",
      "WorktreeExecutionPlane", "McpProxyPlane", "DreamStore", "WorkingMemory",
      "HarnessLoop", "EvalLoopHarness", "judge", "osProfile", "ReplayProvider", "PermissionManager",
      // internalized (kernel boundary / low-level builders)
      "workflowSpecToKernel", "agentRunSpecToKernel", "governancePolicyToKernelEvent",
      "kernelObservationToSessionEvent", "loopInstruction", "buildEvalMessages", "fanoutSynthesize",
      "KERNEL_ROLE_MAP",
    ]) {
      expect(root).not.toHaveProperty(name)
    }
  })
})

describe("subpath barrels", () => {
  it("providers carries backend factories + the base class + profiles", () => {
    for (const n of ["deepseek", "kimi", "qwen", "glm", "minimax", "gemini", "ollama", "OpenAIChatProvider", "endpointProfiles", "CircuitBreaker"])
      expect(providers).toHaveProperty(n)
  })
  it("providers no longer exposes the collapsed dual classes", () => {
    for (const n of ["DeepSeekProvider", "DeepSeekAnthropicProvider", "KimiAnthropicProvider", "MiniMaxOpenAIProvider"])
      expect(providers).not.toHaveProperty(n)
  })
  it("workflow carries orchestration + reducers + contracts", () => {
    for (const n of ["SubAgentOrchestrator", "spawnStandalone", "builtinReducers", "ContractBuilder", "HandoffBus"])
      expect(workflow).toHaveProperty(n)
  })
  it("planes carries the specialized planes", () => {
    for (const n of ["WorktreeExecutionPlane", "ProcessSandboxPlane", "McpProxyPlane", "FileArchiveStore"])
      expect(planes).toHaveProperty(n)
  })
  it("memory carries dream + working memory", () => {
    for (const n of ["WorkingMemory", "InMemoryDreamStore"]) expect(memory).toHaveProperty(n)
  })
  it("harness carries the eval harnesses + judge", () => {
    for (const n of ["AttemptLoop", "RuntimeAttemptBody", "judge"])
      expect(harness).toHaveProperty(n)
    for (const n of ["SinglePassHarness", "EvalLoopHarness", "HarnessLoop", "ContractDrivenHarness"])
      expect(harness).not.toHaveProperty(n)
  })
  it("os carries profiles, signals, permissions, replay-testing", () => {
    for (const n of ["osProfile", "assertNativeProfile", "SignalGateway", "PermissionManager", "ReplayProvider"])
      expect(os).toHaveProperty(n)
  })
})

describe("provider options-object constructor", () => {
  it("constructs OpenAIProvider from an options object with a custom baseURL", () => {
    const p = new OpenAIProvider({ apiKey: "sk-test", model: "mimo-v2.5-pro", baseURL: "https://example.test/v1" })
    expect((p as unknown as { model: string }).model).toBe("mimo-v2.5-pro")
  })
  it("still accepts the legacy positional form", () => {
    const p = new OpenAIProvider("sk-test", "gpt-4o")
    expect((p as unknown as { model: string }).model).toBe("gpt-4o")
  })
})
