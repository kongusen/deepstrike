import { CreatorVerifierMode } from "../src/collaboration/modes/creator-verifier.js"
import type { AgentPool } from "../src/collaboration/pool.js"
import type { VerificationContract } from "../src/collaboration/contract.js"

const contract: VerificationContract = {
  id: "contract-1",
  goal: "ship the fix",
  acceptance: [
    { id: "tests", text: "all tests pass", required: true, weight: 1, machineCheckable: false },
  ],
  antiPatterns: [],
  evidenceRequired: [],
}

describe("CreatorVerifierMode on AttemptLoop", () => {
  it("keeps executor attempts in one session and carries structured verifier feedback", async () => {
    const executions: Array<{ sessionId: string; goal: string; contextInput?: string }> = []
    let attempt = 0
    const pool = {
      ensureCoordinator() { return this },
      async execute(_role: string, input: { sessionId: string; goal: string; contextInput?: string }) {
        executions.push(input)
        attempt++
        return {
          agentId: "executor",
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: `artifact-${attempt}`, toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 10,
          },
        }
      },
      async verify() {
        return JSON.stringify(attempt === 1
          ? {
              passed: false,
              overall_score: 0,
              feedback: "tests are still failing",
              details: [{ criterion: "tests", passed: false, score: 0, feedback: "one failure" }],
            }
          : {
              passed: true,
              overall_score: 1,
              feedback: "all checks pass",
              details: [{ criterion: "tests", passed: true, score: 1, feedback: "verified" }],
            })
      },
    } as unknown as AgentPool

    const outcome = await new CreatorVerifierMode(pool, { maxAttempts: 2 }).run(contract)

    expect(executions).toHaveLength(2)
    expect(executions[0]!.sessionId).toBe(executions[1]!.sessionId)
    expect(executions[0]!.goal).toBe(executions[1]!.goal)
    expect(executions[1]!.contextInput).toBe("tests are still failing")
    expect(outcome).toMatchObject({
      success: true,
      artifact: "artifact-2",
      attemptsUsed: 2,
      totalTokensConsumed: 20,
      checkResults: [{ criterionId: "tests", passed: true, evidence: "verified" }],
    })
  })

  it("rejects free-text verifier output instead of inferring PASS with regexes", async () => {
    const pool = {
      ensureCoordinator() { return this },
      async execute() {
        return {
          agentId: "executor",
          result: {
            termination: "completed",
            finalMessage: { role: "assistant", content: "artifact", toolCalls: [] },
            turnsUsed: 1,
            totalTokensUsed: 1,
          },
        }
      },
      async verify() { return "tests: PASS" },
    } as unknown as AgentPool

    await expect(new CreatorVerifierMode(pool, { maxAttempts: 1 }).run(contract)).rejects.toThrow()
  })
})
