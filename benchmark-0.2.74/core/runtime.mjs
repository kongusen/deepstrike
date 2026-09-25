export function createStubOrchestrator(onCall) {
  return {
    async run(context) {
      onCall?.(context)
      const id = context.manifest.agent_id
      return {
        agentId: id,
        result: {
          termination: "completed",
          finalMessage: { role: "assistant", content: id, toolCalls: [] },
          turnsUsed: 1,
          totalTokensUsed: 1,
        },
      }
    },
  }
}

export function createRunner(sdk, options = {}) {
  return new sdk.advanced.RuntimeRunner({
    sessionLog: options.sessionLog ?? new sdk.advanced.InMemorySessionLog(),
    maxTokens: options.maxTokens ?? 8000,
    subAgentOrchestrator: options.subAgentOrchestrator ?? createStubOrchestrator(options.onCall),
    ...(options.schedulerPolicy ? { schedulerPolicy: options.schedulerPolicy } : {}),
    ...(options.resourceQuota ? { resourceQuota: options.resourceQuota } : {}),
  })
}

export async function collectAsync(iterable) {
  const values = []
  for await (const value of iterable) values.push(value)
  return values
}
