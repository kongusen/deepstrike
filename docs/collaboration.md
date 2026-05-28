# Collaboration Layer

DeepStrike's collaboration layer lets you wire multiple runners together into structured multi-runner workflows. It sits between the RuntimeRunner layer (Layer 1) and your application (Layer 4) as two composable layers of primitives.

---

## Why multi-runner?

A single runner re-using its own context to retry a failed task is vulnerable to **cost-of-maintaining bias** — the model's effort shifts from solving the problem to defending its prior choices. The collaboration layer solves this by separating three concerns across independent runner instances:

| Role | Responsibility | Context policy |
|------|---------------|---------------|
| **Orchestrator** | Produces a `VerificationContract` from a raw goal | Sees only the goal |
| **Executor** | Implements against the contract | Sees goal + contract only; no verifier history |
| **Verifier** | Audits the artifact against the contract | Sees contract + artifact only; no implementation history |

---

## Layer overview

```
┌──────────────────────────────────────────────────────────────┐
│  Layer 4: Application  (FlashNote, MeetingMind, ...)         │
├──────────────────────────────────────────────────────────────┤
│  Layer 3: Collaboration Modes                                 │
│    CreatorVerifierMode │ OrchestrationMode                   │
├──────────────────────────────────────────────────────────────┤
│  Layer 2: Collaboration Primitives  ← this document          │
│    VerificationContract │ AgentPool │ ContractDrivenHarness  │
│    HandoffBus           │ TaskLane                           │
├──────────────────────────────────────────────────────────────┤
│  Layer 1: RuntimeRunner  (SessionLog, ExecutionPlane, DreamStore) │
├──────────────────────────────────────────────────────────────┤
│  Layer 0: Kernel  (ContextManager, TaskGraph, EvalPipeline)  │
└──────────────────────────────────────────────────────────────┘
```

---

## VerificationContract

A `VerificationContract` is the central coordination object. It defines *what correct looks like* before execution starts — decoupling the definition of success from its implementation.

Contracts live in the executor's **Slot 1** (`system_stable` / system partition), which the kernel never compresses. The verifier receives the same contract alongside the artifact.

### Structure

```typescript
interface VerificationContract {
  id: string                       // stable id, doubles as skill name on extraction
  goal: string                     // goal injected into the executor's context
  acceptance: AcceptanceCriterion[] // ordered list of pass/fail criteria
  antiPatterns: string[]           // patterns the executor must avoid
  evidenceRequired: string[]       // artifacts that must exist before verification
}

interface AcceptanceCriterion {
  id: string
  text: string
  required: boolean    // required=true → failure here fails the whole contract
  weight: number       // contribution to weighted score [0.0–1.0]
  machineCheckable: boolean  // true → SDK can verify deterministically
}
```

### Building a contract

**Node.js**
```typescript
import { ContractBuilder } from "@deepstrike/sdk"

const contract = new ContractBuilder("report-v1", "Write a research report on X")
  .criterion("has-sources", "Report cites at least 3 sources", { weight: 0.4 })
  .criterion("word-count",  "Report is 500–2000 words",        { weight: 0.2, machineCheckable: true })
  .criterion("no-hallucination", "All claims are traceable to cited sources", { weight: 0.4 })
  .antiPattern("Do not fabricate citations")
  .evidence("Final report text")
  .evidence("Citation list")
  .build()
```

**Python**
```python
from deepstrike import ContractBuilder

contract = (ContractBuilder("report-v1", "Write a research report on X")
    .criterion("has-sources", "Report cites at least 3 sources", weight=0.4)
    .criterion("word-count",  "Report is 500–2000 words",        weight=0.2, machine_checkable=True)
    .criterion("no-hallucination", "All claims are traceable to cited sources", weight=0.4)
    .anti_pattern("Do not fabricate citations")
    .evidence("Final report text")
    .evidence("Citation list")
    .build())
```

### Injecting a contract into the kernel

The `LoopStateMachine.setContract()` method formats the contract as Markdown and pushes it to the **system partition** (Slot 1):

```typescript
// @deepstrike/core (low-level FFI)
sm.setContract(contract)   // must be called before sm.start()

// or use the helper if you're managing the loop manually:
import { formatContractForSystemPrompt } from "@deepstrike/sdk"
const text = formatContractForSystemPrompt(contract)
sm.addSystemMessage(text, Math.ceil(text.length / 4))
```

---

## AgentPool

`AgentPool` manages a set of role-specific `RuntimeRunner` instances. The key invariant: **each role has its own runner with its own session log partition**. The verifier never sees what the executor wrote.

```typescript
import { AgentPool } from "@deepstrike/sdk"

const pool = new AgentPool()
  .add("executor", executorRunner)
  .add("verifier", verifierRunner)       // no tools — verifier must only read, never act
  .add("orchestrator", orchestratorRunner)
```

**Pool API:**

| Method | Description |
|--------|-------------|
| `pool.add(role, agent)` | Register an agent for a role; returns `this` for chaining |
| `pool.get(role)` | Get the agent for a role; throws if not registered |
| `pool.has(role)` | Check if a role is registered |
| `pool.runVerifier(ctx)` | Run the verifier with an isolated context (artifact + contract) |
| `pool.runOrchestrator(goal)` | Run the orchestrator to produce a contract JSON |

---

## ContractDrivenHarness

The core multi-agent execution primitive. It runs the executor and verifier through a structured protocol on each attempt.

```
attempt N:
  executor.run(goal + contract)           → artifact
  verifier.runIsolated(artifact, contract) → audit text
  parse audit → ContractCheckResult[]
  all required criteria pass → Done (success)
  violations found → inject violation list into next executor goal
  maxAttempts exceeded → HandoffArtifact with blocked_on
```

**Key difference from `HarnessLoop`:**

| | `HarnessLoop` | `ContractDrivenHarness` |
|--|--------------|------------------------|
| Executor / Verifier | Same agent instance | Two separate instances |
| Verifier context | Sees full transcript | Sees artifact + contract only |
| Feedback | Free-text LLM summary | Structured `Violation[]` list |
| Contract type | `string[]` criteria | `VerificationContract` |

### Usage

**Node.js**
```typescript
import { AgentPool, ContractDrivenHarness, ContractBuilder } from "@deepstrike/sdk"

const contract = new ContractBuilder("sort-fn", "Write a Python sort function")
  .criterion("correct",      "Function sorts correctly for all inputs")
  .criterion("has-docstring","Function has a docstring")
  .criterion("handles-empty","Function handles an empty list")
  .build()

const pool = new AgentPool()
  .add("executor", executorRunner)
  .add("verifier", verifierRunner)

const harness = new ContractDrivenHarness(pool, contract, { maxAttempts: 3 })
const outcome = await harness.run()

console.log(outcome.success)           // true / false
console.log(outcome.attemptsUsed)      // 1–3
console.log(outcome.checkResults)      // ContractCheckResult[]
console.log(outcome.handoff)           // HandoffArtifact
```

**Python**
```python
from deepstrike import (AgentPool, ContractDrivenHarness,
                        ContractBuilder, ContractHarnessOptions)

contract = (ContractBuilder("sort-fn", "Write a Python sort function")
    .criterion("correct",      "Function sorts correctly for all inputs")
    .criterion("has-docstring","Function has a docstring")
    .criterion("handles-empty","Function handles an empty list")
    .build())

pool = AgentPool().add("executor", executor_runner).add("verifier", verifier_runner)

harness = ContractDrivenHarness(pool, contract, ContractHarnessOptions(max_attempts=3))
outcome = await harness.run()

print(outcome.success)           # True / False
print(outcome.attempts_used)     # 1–3
print(outcome.check_results)     # list[ContractCheckResult]
print(outcome.handoff)           # HandoffArtifact
```

---

## HandoffBus

`HandoffBus` is the canonical factory for `HandoffArtifact`. Every transition between agent contexts — harness completion, sub-agent result, dream consolidation — produces a `HandoffArtifact` through one of its static methods.

```typescript
interface HandoffArtifact {
  goal: string
  sprint: number
  progressSummary: string
  openTasks: string[]
  contractStatus: ContractCheckResult[]   // what has been proven
  driftRate24h: number                    // failed / total over 24 h
  blockedOn: string[]                     // issues needing escalation
}
```

The invariant: **a handoff tells the next agent not only what was done, but what has been proven.**

### Factory methods

```typescript
import { HandoffBus } from "@deepstrike/sdk"

// From a ContractDrivenHarness run
const handoff = HandoffBus.fromContractOutcome({ contract, checkResults, artifact, success })

// From a sub-agent's final message
const handoff = HandoffBus.fromSubAgentResult({ goal, finalMessage, sprint: 2 })

// From a dream consolidation
const handoff = HandoffBus.fromDream({ goal, dreamResult })

// Render as a compact context injection
const note = HandoffBus.toContextNote(handoff)
// "[Handoff from sprint 1]\nGoal: ...\nProgress: ...\nContract: 3/3 criteria passed"

// Escalation check
if (HandoffBus.requiresEscalation(handoff, { driftThreshold: 0.05 })) {
  // pause autonomous delegation — drift > 5% or blocked_on is non-empty
}
```

---

## Collaboration Modes

Modes are declarative wiring patterns for multiple agents. They compose the primitives above.

### CreatorVerifierMode

The simplest mode — two agents, one contract. Wraps `ContractDrivenHarness` and accumulates drift metrics across runs.

**Node.js**
```typescript
import { AgentPool, CreatorVerifierMode, ContractBuilder, HandoffBus } from "@deepstrike/sdk"

const pool = new AgentPool()
  .add("executor", executorRunner)
  .add("verifier", verifierRunner)

const mode = new CreatorVerifierMode(pool, { maxAttempts: 3 })

const outcome = await mode.run(contract)

// Drift monitoring
const metrics = mode.getMetrics()
// { total: 5, failed: 1, driftRate: 0.2 }

if (mode.isDrifting(0.05)) {
  // escalate — >5% of runs are failing required criteria
}
```

**Python**
```python
from deepstrike import AgentPool, CreatorVerifierMode, HandoffBus

pool = AgentPool().add("executor", executor_runner).add("verifier", verifier_runner)
mode = CreatorVerifierMode(pool, max_attempts=3)

outcome = await mode.run(contract)

metrics = mode.get_metrics()   # CreatorVerifierMetrics(total=5, failed=1, drift_rate=0.2)
if mode.is_drifting(0.05):
    pass  # escalate
```

### OrchestrationMode

Three-role mode — the orchestrator produces a `VerificationContract` from a raw goal, then `CreatorVerifierMode` executes it. Requires all three roles in the pool.

**Node.js**
```typescript
import { AgentPool, OrchestrationMode } from "@deepstrike/sdk"

const pool = new AgentPool()
  .add("orchestrator", reasonerRunner)   // strong reasoning model
  .add("executor",     executorRunner)
  .add("verifier",     verifierRunner)

const mode = new OrchestrationMode(pool)

// Pass a raw goal — the orchestrator produces the contract
const { outcome, contract } = await mode.run("Write a market analysis for the EV sector")

console.log(contract.id)           // orchestrated contract id
console.log(outcome.success)
console.log(outcome.handoff)
```

**Python**
```python
from deepstrike import AgentPool, OrchestrationMode

pool = (AgentPool()
    .add("orchestrator", reasoner_runner)
    .add("executor",     executor_runner)
    .add("verifier",     verifier_runner))

mode = OrchestrationMode(pool)
outcome, contract = await mode.run("Write a market analysis for the EV sector")

print(contract.id)       # orchestrated contract id
print(outcome.success)
```

---

## TaskLane

`TaskLane` is a scheduling hint on `RuntimeTask` that tells the executor how to parallelise work within a `TaskGraph`. It is a kernel-level type available to all SDKs.

| Lane | Parallelism | Typical use |
|------|------------|-------------|
| `orchestrate` | Serial (1 at a time) | Produces contracts; must complete before execute |
| `implement` | Serial | Code / content generation; enforced by `executor::next_batch` |
| `retrieve` | Parallel | Web search, knowledge retrieval, memory queries |
| `verify` | Parallel, but context-isolated | Independent contract checks |

```typescript
// @deepstrike/core — low-level API
const graph = new TaskGraph([
  { goal: "Define research contract", lane: "orchestrate", dependsOn: [] },
  { goal: "Search source A",         lane: "retrieve",    dependsOn: [0] },
  { goal: "Search source B",         lane: "retrieve",    dependsOn: [0] },
  { goal: "Write report",            lane: "implement",   dependsOn: [1, 2] },
  { goal: "Audit report",            lane: "verify",      dependsOn: [3] },
])
```

---

## Escalation and drift monitoring

A `HandoffArtifact` carries a `driftRate24h` field — the ratio of verification failures over a 24-hour window. When drift exceeds the threshold (default 5%), `HandoffBus.requiresEscalation()` returns true.

```typescript
// After each mode.run():
if (HandoffBus.requiresEscalation(outcome.handoff, { driftThreshold: 0.05 })) {
  // pause autonomous delegation
  // surface blocked_on list to human or orchestrator
  console.log("Blocked on:", outcome.handoff.blockedOn)
}
```

Combine with `CreatorVerifierMode.getMetrics()` for per-mode tracking across many runs.

---

## Choosing the right API

| Scenario | Recommended API |
|----------|----------------|
| Single agent, custom retry logic | `EvalLoopHarness` with `QualityGate` |
| Single agent, LLM-as-judge retry | `HarnessLoop` |
| Two agents, contract-driven | `ContractDrivenHarness` directly |
| Two agents, contract-driven + metrics | `CreatorVerifierMode` |
| Three agents, goal → contract → execute → verify | `OrchestrationMode` |
| Low-level task scheduling | `TaskGraph` + `TaskLane` |

---

## Model selection by role

Role-specific model selection improves quality and reduces cost:

| Role | Model characteristics | Example |
|------|----------------------|---------|
| Orchestrator | Strong reasoning; low throughput is acceptable | `claude-opus-4-7`, DeepSeek R1 |
| Executor | Code + task proficiency; high context | `claude-sonnet-4-6`, GPT-4o |
| Verifier | Low temperature (0.0–0.1); no tools | Any capable model, deterministic settings |

```typescript
const pool = new AgentPool()
  .add("orchestrator", orchestratorRunner) // strong reasoning; small tool surface
  .add("executor", executorRunner)         // larger context; task tools registered
  .add("verifier", verifierRunner)         // deterministic; no mutating tools
```
