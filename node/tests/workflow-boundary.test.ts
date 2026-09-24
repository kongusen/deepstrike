import { workflowNodeSpecToKernel, workflowSpecToKernel } from "../src/types/agent.js"
import { dynamicWorkflowPlanToKernel, dynamicWorkflowReplayFactToKernel } from "../src/workflow/dynamic.js"
import { workflowBudgetFromKernel, workflowSpawnNodeFromKernel } from "../src/runtime/kernel-step.js"

describe("workflow host-to-kernel boundary", () => {
  it("projects node fields and control-flow kind into the kernel shape", () => {
    expect(workflowNodeSpecToKernel({
      nodeId: "step-1",
      agent: "worker",
      task: "implement",
      role: "implement",
      isolation: "read_only",
      contextInheritance: "system_only",
      modelHint: "fast",
      tokenBudget: 100,
      dependsOn: [0],
      depPolicy: "accept_partial",
      loop: { maxIters: 2 },
    })).toEqual({
      task: { goal: "implement", criteria: [] },
      role: "implement",
      isolation: "read_only",
      context_inheritance: "system_only",
      model_hint: "fast",
      kind: { type: "loop", max_iters: 2 },
      token_budget: 100,
      depends_on: [0],
      dep_policy: "accept_partial",
    })
  })

  it("composes a workflow root from the same node projection", () => {
    expect(workflowSpecToKernel({
      nodes: [{ task: "plan", role: "plan" }],
    })).toEqual({
      nodes: [{
        task: { goal: "plan", criteria: [] },
        role: "plan",
        isolation: "shared",
        context_inheritance: "none",
        dep_policy: "all_success",
      }],
    })
  })

  it("projects kernel spawn bookkeeping into the host runner shape", () => {
    expect(workflowSpawnNodeFromKernel({
      agent_id: "wf-node0",
      goal: "implement",
      role: "worker",
      isolation: "shared",
      context_inheritance: "none",
      task_id: "task-0",
      attempt_id: "attempt-0",
      launch_token: "opaque",
      node_id: "node-0",
      reducer: "join",
      input_agent_ids: ["wf-node1"],
      token_budget: 100,
    })).toEqual({
      agent_id: "wf-node0",
      goal: "implement",
      role: "worker",
      isolation: "shared",
      context_inheritance: "none",
      reducer: "join",
      input_agent_ids: ["wf-node1"],
      token_budget: 100,
    })
  })

  it("keeps the kernel budget snapshot typed at the host boundary", () => {
    expect(workflowBudgetFromKernel({ nodes_remaining: 2, max_total_tokens: "5000", max_concurrency: 2 })).toEqual({
      nodes_remaining: 2,
      max_total_tokens: "5000",
      max_concurrency: 2,
    })
  })

  it("projects dynamic workflow plans and replay facts into canonical wire shapes", () => {
    expect(dynamicWorkflowPlanToKernel({
      runId: "run-1",
      sequence: 3,
      nodes: [{ nodeId: "node-1", dependsOn: ["node-0"], promptFingerprint: "fp", replay: "executed" }],
    })).toEqual({
      run_id: "run-1",
      sequence: 3,
      nodes: [{ node_id: "node-1", depends_on: ["node-0"], prompt_fingerprint: "fp", replay: "executed" }],
    })
    expect(dynamicWorkflowReplayFactToKernel({
      runId: "run-1",
      sequence: 3,
      nodeId: "node-1",
      promptFingerprint: "fp",
      status: "completed",
      replay: "reused",
      resultDigest: "digest",
      termination: "completed",
    })).toEqual({
      run_id: "run-1",
      sequence: 3,
      node_id: "node-1",
      prompt_fingerprint: "fp",
      status: "completed",
      replay: "reused",
      result_digest: "digest",
      termination: "completed",
    })
  })
})
