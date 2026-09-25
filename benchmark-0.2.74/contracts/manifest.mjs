export const PUBLIC_CONTRACTS = [
  {
    id: "root.intent",
    surface: "root",
    required: [
      "createAgent", "tool", "streamingTool", "safeTool", "ok", "fail",
      "AnthropicProvider", "OpenAIProvider", "OpenAIResponsesProvider", "createProvider",
      "createWorkflow", "evaluate",
    ],
    forbidden: [
      "RuntimeRunner", "LocalExecutionPlane", "InMemorySessionLog", "FileSessionLog",
      "runAgent", "runFanout", "ReplayProvider", "PermissionManager", "WorkingMemory",
      "judge", "osProfile", "WorktreeExecutionPlane", "McpProxyPlane",
    ],
  },
  {
    id: "providers.backend",
    surface: "providers",
    required: ["deepseek", "kimi", "qwen", "glm", "minimax", "gemini", "ollama", "OpenAIChatProvider", "endpointProfiles", "modelRegistry", "CircuitBreaker"],
  },
  {
    id: "workflow.orchestration",
    surface: "workflow",
    required: [
      "SubAgentOrchestrator", "spawnStandalone", "builtinReducers", "createWorkflow", "lowerWorkflowDefinition",
      "DynamicWorkflowControl", "dynamicAgentTask", "InMemoryDynamicWorkflowReplayStore", "FileDynamicWorkflowReplayStore",
      "ContractBuilder", "HandoffBus", "startWorkflowTool", "submitWorkflowNodesTool",
    ],
  },
  {
    id: "planes.execution",
    surface: "planes",
    required: ["WorktreeExecutionPlane", "ProcessSandboxPlane", "McpProxyPlane", "FilteredExecutionPlane", "FileArchiveStore", "InMemoryCredentialVault"],
  },
  {
    id: "memory.boundary",
    surface: "memory",
    required: ["WorkingMemory", "DurableMemory", "InMemoryMemoryStore", "rankMemories", "extractSessionMemories", "parseExtractedMemories"],
  },
  {
    id: "harness.evaluation",
    surface: "harness",
    required: ["AttemptLoop", "RuntimeAttemptBody", "VerdictFnJudge", "LlmEvalJudge", "HybridJudge", "judge", "composeSystemPrompt", "manifestDigest", "NudgeEngine"],
  },
  {
    id: "os.replay",
    surface: "os",
    required: ["osProfile", "assertNativeProfile", "SignalGateway", "PermissionManager", "ReplayProvider", "extractRecordedMessages", "primitiveForKind"],
  },
  {
    id: "advanced.runtime",
    surface: "advanced",
    required: ["RuntimeRunner", "LocalExecutionPlane", "InMemorySessionLog", "FileSessionLog", "runAgent", "runFanout", "collectText"],
  },
  {
    id: "runtime.host",
    surface: "runtime",
    required: ["RuntimeRunner", "ContextManager", "InMemorySessionLog", "FileSessionLog", "runAgent", "runFanout"],
  },
  {
    id: "evals.trace",
    surface: "evals",
    required: ["evaluate", "judge", "buildEvalMessages", "parseVerdict", "verdictOutputSchema"],
  },
]

export function checkPublicContracts(sdk, contracts = PUBLIC_CONTRACTS) {
  const failures = []
  const results = []
  for (const contract of contracts) {
    const module = sdk.surfaces?.[contract.surface]
    const missing = module ? contract.required.filter(name => !(name in module)) : [...contract.required]
    const forbiddenPresent = module ? (contract.forbidden ?? []).filter(name => name in module) : []
    const passed = missing.length === 0 && forbiddenPresent.length === 0
    results.push({ id: contract.id, surface: contract.surface, passed, missing, forbiddenPresent })
    if (!passed) failures.push({ id: contract.id, surface: contract.surface, missing, forbiddenPresent })
  }
  return { passed: failures.length === 0, failures, results }
}

export function assertPublicContracts(sdk, contracts = PUBLIC_CONTRACTS) {
  const report = checkPublicContracts(sdk, contracts)
  if (!report.passed) {
    const detail = report.failures.map(f => `${f.id}: missing=[${f.missing.join(",")}] forbidden=[${f.forbiddenPresent.join(",")}]`).join("; ")
    throw new Error(`public contract failure: ${detail}`)
  }
  return report
}
