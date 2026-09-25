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

export const EXPECTED_EXPORTS = {
  ".": { import: "./dist/index.js", types: "./dist/index.d.ts" },
  "./providers": { import: "./dist/providers/public.js", types: "./dist/providers/public.d.ts" },
  "./workflow": { import: "./dist/workflow/public.js", types: "./dist/workflow/public.d.ts" },
  "./planes": { import: "./dist/planes/public.js", types: "./dist/planes/public.d.ts" },
  "./memory": { import: "./dist/memory/public.js", types: "./dist/memory/public.d.ts" },
  "./harness": { import: "./dist/harness/public.js", types: "./dist/harness/public.d.ts" },
  "./os": { import: "./dist/os/public.js", types: "./dist/os/public.d.ts" },
  "./advanced": { import: "./dist/advanced/public.js", types: "./dist/advanced/public.d.ts" },
  "./runtime": { import: "./dist/runtime/public.js", types: "./dist/runtime/public.d.ts" },
  "./evals": { import: "./dist/evals/public.js", types: "./dist/evals/public.d.ts" },
}

export function checkPackageExportMap(sdk, expected = EXPECTED_EXPORTS) {
  const actual = sdk.packageJson?.exports ?? {}
  const missing = Object.keys(expected).filter(path => !(path in actual))
  const wrong = Object.entries(expected).flatMap(([path, contract]) => {
    const value = actual[path]
    if (!value) return []
    return ["import", "types"].filter(condition => value[condition] !== contract[condition]).map(condition => ({
      path,
      condition,
      expected: contract[condition],
      actual: value[condition],
    }))
  })
  return { passed: missing.length === 0 && wrong.length === 0, missing, wrong }
}

export function checkPublicContracts(sdk, contracts = PUBLIC_CONTRACTS) {
  const failures = []
  const results = []
  const exportMap = checkPackageExportMap(sdk)
  results.push({ id: "package.export-map", surface: "package.json", passed: exportMap.passed, missing: exportMap.missing, wrong: exportMap.wrong })
  if (!exportMap.passed) failures.push({ id: "package.export-map", surface: "package.json", missing: exportMap.missing, wrong: exportMap.wrong })
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
    const detail = report.failures.map(f => `${f.id}: missing=[${(f.missing ?? []).join(",")}] forbidden=[${(f.forbiddenPresent ?? []).join(",")}] wrong=[${(f.wrong ?? []).map(item => `${item.path}.${item.condition}`).join(",")}]`).join("; ")
    throw new Error(`public contract failure: ${detail}`)
  }
  return report
}
