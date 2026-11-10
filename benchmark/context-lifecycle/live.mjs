#!/usr/bin/env node
// Live check for plan task 3.1: does a short dependency preview cost answer quality?
//
//   node benchmark/context-lifecycle/live.mjs [--repo <checkout>] [--mode full|handle] [--tasks N]
//        [--provider openai|deepseek] [--variant <name>] [--out trials.jsonl]
//
// Three explore nodes return fixed 16k-character survey reports without calling a model; only the
// synthesize node runs on the real provider from `.env` (OPENAI_* or DEEPSEEK_*). Each task asks for
// one access code that sits inside the 600-character preview, inside the 8,000-character clip, or
// beyond it. The answer is graded by exact match. Writes one trial per task for `deepstrike-lab`
// (scenario `fan-out`; the variant defaults to the mode); never prints a key.

import { resolve } from "node:path"
import { MeteredProvider, here, loadEnv, parseArgs, realProvider, recordingSessionLog, redact, sdk, trialFrom, writeTrials } from "./trial.mjs"

const arg = parseArgs()
const repo = resolve(arg("--repo", here))
const mode = arg("--mode", "full")
const taskCount = Number(arg("--tasks", "10"))
const variant = arg("--variant", mode)
const out = arg("--out")

await loadEnv()
const dist = sdk(repo)
const { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } = await dist("advanced/public.js")
const { defaultSubAgentOrchestrator } = await dist("workflow/public.js")
const { provider: inner, model } = await realProvider(dist, arg("--provider", "openai"))
const provider = new MeteredProvider(inner)

// ── fixtures ───────────────────────────────────────────────────────────────────────────────────

const ROWS = 200
const code = (area, row) => `${String.fromCharCode(65 + ((area * 7 + row) % 26))}${String.fromCharCode(65 + ((row * 3 + area) % 26))}-${String(1000 + ((area * 7919 + row * 104729) % 9000)).padStart(4, "0")}`
const report = area => `Survey report for area-${area}.\n` + Array.from({ length: ROWS }, (_, row) =>
  `station ${area}-${String(row).padStart(3, "0")}: status nominal, access code ${code(area, row)}, last check day ${row % 28 + 1}`,
).join("\n")

// Rows 5 sit inside a 600-char preview; rows 40 inside the 8,000-char clip; rows 150 beyond it.
const DEPTHS = [5, 40, 150]
const TASKS = Array.from({ length: taskCount }, (_, i) => {
  const area = (i % 3) + 1
  const row = DEPTHS[i % DEPTHS.length]
  return { id: `t${i}`, area, row, depth: row === 5 ? "preview" : row === 40 ? "clip" : "beyond", expected: code(area, row) }
})

// ── one task ───────────────────────────────────────────────────────────────────────────────────

async function runTask(task) {
  provider.reset()
  const recorder = recordingSessionLog(new InMemorySessionLog())
  let answer = ""
  let termination
  const orchestrator = {
    async run(ctx) {
      const area = /survey area-(\d)/.exec(ctx.spec.goal)?.[1]
      if (area && !ctx.spec.goal.includes("Answer")) {
        return {
          agentId: ctx.spec.identity.agentId,
          result: { termination: "completed", finalMessage: { role: "assistant", content: report(Number(area)), toolCalls: [] }, turnsUsed: 1, totalTokensUsed: 0 },
        }
      }
      const result = await defaultSubAgentOrchestrator.run(ctx)
      const content = result.result.finalMessage?.content
      answer = typeof content === "string" ? content : JSON.stringify(content ?? "")
      termination = result.result.termination
      return result
    },
  }
  const runner = new RuntimeRunner({
    provider,
    sessionLog: recorder.log,
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 64_000,
    maxTurns: 6,
    maxTotalTokens: 60_000,
    subAgentOrchestrator: orchestrator,
  })
  const question = `Answer from the three survey reports: what is the access code of station ${task.area}-${String(task.row).padStart(3, "0")}? Reply with the code only.`
  let error
  try {
    await runner.runWorkflow({
      nodes: [
        { task: "survey area-1", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: "survey area-2", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: "survey area-3", role: "explore", isolation: "read_only", contextInheritance: "system_only" },
        { task: question, role: "plan", dependsOn: [0, 1, 2], context: { dependencyMode: mode } },
      ],
    }, { sessionId: `live-${task.id}` })
  } catch (cause) {
    error = redact(cause?.message ?? cause)
  }
  error ??= provider.lastError ?? (termination && termination !== "completed" ? `termination: ${termination}` : undefined)
  return trialFrom({
    scenario: "fan-out",
    variant,
    task: `${task.id}-${task.depth}`,
    success: answer.includes(task.expected),
    error,
    provider,
    events: recorder.events,
    model,
  })
}

// Each trial is written as soon as it finishes, so an interrupted run keeps what it measured.
const trials = []
for (const task of TASKS) {
  const trial = await runTask(task)
  trials.push(trial)
  await writeTrials(out, [trial])
  console.error(`${trial.task}: ${trial.success ? "correct" : "wrong"} ${trial.cost.calls} calls ${trial.cost.sent_chars} chars${trial.error ? ` (${trial.error})` : ""}`)
}
console.error(`${variant} on ${model}: ${trials.filter(trial => trial.success).length}/${trials.length} correct`)
