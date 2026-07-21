/**
 * Self-Harness fixture TaskAdapter — deterministic, zero-LLM, zero-runner.
 *
 * The CI/e2e adapter for the propose→validate→promote loop. Every task's pass/fail is a pure
 * function of the HarnessManifest handed to `runTask(task, manifest)` — no provider, no `RuntimeRunner`,
 * no clock, no randomness — so a full loop run is byte-for-byte reproducible and costs nothing. Failing
 * tasks emit synthetic event streams shaped exactly like a bench `*.events.json` dump (`{seq, event}[]`
 * with `run_terminal` and `tool_completed` `is_error` results) plus a matching `Verdict`, so the
 * evidence pipeline (`extractFailureRecord` / `failureSignature` / `clusterFailures` / `renderExcerpt`)
 * consumes them unchanged.
 *
 * The task set deliberately spans the addressability axis the loop must navigate:
 *   - `verify-keyword`  passes ONLY when `instructions.verification` mentions "run tests"  (addressable)
 *   - `nudge-guard`     passes ONLY when a `tool_error` nudge rule is present               (addressable)
 *   - `exec-cite`       passes ONLY when `instructions.execution` mentions "cite sources"   (seed passes;
 *                                                                                            a regression
 *                                                                                            edit breaks it)
 *   - `ceiling`         ALWAYS fails — models a model capability ceiling, not harness-addressable
 *   - `stable-1/2/3`    ALWAYS pass — behavior the loop must preserve
 *
 * Approved deviation from the adapter contract: the adapter interface is `runTask(task, manifest)` (not
 * `runTask(task, runtimeOptionsPatch)`). The fixture adapter inspects the manifest directly to decide
 * pass/fail; the live adapter (see live.mjs) is the one that builds base RuntimeOptions and `applyManifest`s.
 *
 * @typedef {import("../../utils/sdk.mjs").ProviderDescriptor} ProviderDescriptor
 * @typedef {import("../evidence.mjs").Verdict} Verdict
 * @typedef {import("../evidence.mjs").Criterion} Criterion
 * @typedef {import("../trace-excerpt.mjs").EventEnvelope} EventEnvelope
 *
 * @typedef {Object} Task
 * @property {string} id
 * @property {string} goal
 * @property {Criterion[]} criteria
 * @property {number} [maxTurns]
 *
 * @typedef {Object} RunOutcome
 * @property {boolean} passed
 * @property {Verdict} verdict
 * @property {EventEnvelope[]} events    Bench `{seq, event}[]` shape.
 * @property {string} termination        run_terminal.reason.
 *
 * @typedef {Object} TaskAdapter
 * @property {string} id
 * @property {() => Task[]} listTasks
 * @property {(task: Task, manifest: import("../../../node/src/harness/manifest.js").HarnessManifest) => Promise<RunOutcome>} runTask
 */

/** The full editable-surface vocabulary a seed manifest opens to the proposer. `runtime.allowedToolIds`
 *  is opened so the loop can discover tool-routing edits; it is intersection-only at the runner. */
export const EDITABLE_SURFACES = [
  "instructions.bootstrap",
  "instructions.execution",
  "instructions.verification",
  "instructions.failureRecovery",
  "nudges",
  "runtime.maxTurns",
  "runtime.maxTotalTokens",
  "runtime.allowedToolIds",
]

/** The distractor tool id the `tool-route` task fails on until the manifest narrows it away. */
export const DISTRACTOR_TOOL_ID = "distracting_search"

/** Case-insensitive substring test over a possibly-undefined instruction slot. */
function mentions(text, keyword) {
  return typeof text === "string" && text.toLowerCase().includes(keyword.toLowerCase())
}

/** True when the manifest carries a nudge rule whose trigger kind matches. */
function hasNudgeKind(manifest, kind) {
  return Array.isArray(manifest?.nudges) && manifest.nudges.some(r => r && r.on && r.on.kind === kind)
}

/**
 * True when `runtime.allowedToolIds` is a NON-EMPTY list that excludes `toolId`. Unset ⇒ false (the
 * runner would expose all tools, distractor included); a list still carrying `toolId` ⇒ false. This is
 * the fixture analog of the intersection ceiling: the only way to pass is to actively narrow the tool out.
 */
function narrowsAwayTool(manifest, toolId) {
  const ids = manifest?.runtime?.allowedToolIds
  return Array.isArray(ids) && ids.length > 0 && !ids.includes(toolId)
}

// ── Fixture task specifications ───────────────────────────────────────────────
// Each spec has a pure `decide(manifest) -> boolean` and, when failing, a `failure` profile the
// synthetic event/verdict builders render. Failure profiles are chosen so distinct mechanisms cluster
// into distinct signatures (the evidence pipeline clusters by EXACT signature match).

/**
 * @typedef {Object} FailureProfile
 * @property {string} termination                              run_terminal.reason.
 * @property {number} turns
 * @property {number} tokens
 * @property {string[]} failedCriteriaIds                      criterion ids the verdict marks unpassed.
 * @property {Array<{tool: string, args?: string, error_kind?: string, denied?: boolean, reason?: string}>} [steps]
 * @property {string} [finalNote]                              closing assistant line.
 */

/** @type {Array<Task & { decide: (m: any) => boolean, failure?: FailureProfile }>} */
const TASK_SPECS = [
  {
    id: "verify-keyword",
    goal: "Apply the requested refactor and confirm it is correct.",
    criteria: [
      { id: "VK_apply", text: "the agent applies the requested refactor" },
      { id: "VK_verify", text: "the agent verifies the change by running tests" },
    ],
    // Addressable: only passes once the verification instruction slot tells the model to run tests.
    decide: m => mentions(m?.instructions?.verification, "run tests"),
    failure: {
      termination: "completed",
      turns: 2,
      tokens: 300,
      failedCriteriaIds: ["VK_verify"],
      steps: [{ tool: "edit_file", args: '{"path":"src/app.js"}' }],
      finalNote: "Applied the edit. Done.",
    },
  },
  {
    id: "nudge-guard",
    goal: "Recover after a tool error and still complete the task.",
    criteria: [
      { id: "NG_recover", text: "the agent recovers after the first tool error" },
    ],
    // Addressable: only passes when a tool_error nudge exists to prompt recovery.
    decide: m => hasNudgeKind(m, "tool_error"),
    failure: {
      termination: "no_progress",
      turns: 3,
      tokens: 420,
      failedCriteriaIds: ["NG_recover"],
      steps: [
        { tool: "read_file", args: '{"path":"src/missing.js"}', error_kind: "file_not_found" },
        { tool: "read_file", args: '{"path":"src/missing.js"}', error_kind: "file_not_found" },
      ],
      finalNote: "Kept hitting the same missing file and gave up.",
    },
  },
  {
    id: "tool-route",
    goal: "Investigate the reported issue using only the tools that matter.",
    criteria: [
      { id: "TR_focus", text: "the agent stays on the core tools and does not burn turns on the distractor" },
    ],
    // Addressable via a TOOL surface: the scripted provider keeps calling the distractor tool until it
    // runs out of turns. Passes ONLY once the manifest narrows allowedToolIds to exclude the distractor.
    decide: m => narrowsAwayTool(m, DISTRACTOR_TOOL_ID),
    failure: {
      termination: "no_progress",
      turns: 5,
      tokens: 800,
      failedCriteriaIds: ["TR_focus"],
      steps: [
        { tool: DISTRACTOR_TOOL_ID, args: '{"q":"unrelated tangent"}' },
        { tool: DISTRACTOR_TOOL_ID, args: '{"q":"another tangent"}' },
        { tool: DISTRACTOR_TOOL_ID, args: '{"q":"still off track"}' },
      ],
      finalNote: "Kept calling the distractor tool and never made progress.",
    },
  },
  {
    id: "exec-cite",
    goal: "Answer the research question with grounded citations.",
    criteria: [
      { id: "EC_answer", text: "the agent answers the research question" },
      { id: "EC_cite", text: "the agent cites its sources" },
    ],
    // Seed passes (execution slot mentions "cite sources"); a regression that clears execution breaks it.
    decide: m => mentions(m?.instructions?.execution, "cite sources"),
    failure: {
      termination: "completed",
      turns: 2,
      tokens: 260,
      failedCriteriaIds: ["EC_cite"],
      steps: [{ tool: "search", args: '{"q":"topic"}' }],
      finalNote: "Answered but did not attach citations.",
    },
  },
  {
    id: "ceiling",
    goal: "Solve the intentionally unsolvable optimization within the turn budget.",
    criteria: [{ id: "CL_solve", text: "the agent solves the optimization" }],
    // NOT addressable — always fails regardless of the harness (capability ceiling / task noise).
    decide: () => false,
    failure: {
      termination: "max_turns",
      turns: 6,
      tokens: 1200,
      failedCriteriaIds: ["CL_solve"],
      steps: [
        { tool: "solve", args: '{"n":1}', error_kind: "tool_timeout" },
        { tool: "solve", args: '{"n":2}', error_kind: "tool_timeout" },
      ],
      finalNote: "Could not converge; the problem appears infeasible.",
    },
  },
  { id: "stable-1", goal: "Echo the provided value back verbatim.", criteria: [{ id: "S1_echo", text: "the agent echoes the value" }], decide: () => true },
  { id: "stable-2", goal: "Report the current step count.", criteria: [{ id: "S2_report", text: "the agent reports the count" }], decide: () => true },
  { id: "stable-3", goal: "Acknowledge the instruction.", criteria: [{ id: "S3_ack", text: "the agent acknowledges" }], decide: () => true },
]

// ── Synthetic event + verdict builders ────────────────────────────────────────

/**
 * Build a bench-shaped `{seq, event}[]` stream from a task + outcome.
 * @param {Task} task
 * @param {{ termination: string, turns: number, tokens: number, steps?: FailureProfile["steps"], finalNote?: string }} profile
 * @returns {EventEnvelope[]}
 */
function synthEvents(task, profile) {
  /** @type {EventEnvelope[]} */
  const events = []
  let seq = 0
  const push = event => events.push({ seq: seq++, event })

  push({
    kind: "run_started",
    run_id: `${task.id}-0000`,
    goal: task.goal,
    criteria: task.criteria.map(c => c.text),
    system_prompt: "You are an agent working under a self-improving harness.",
  })

  let turn = 0
  for (const step of profile.steps ?? []) {
    const callId = `call_${task.id}_${turn}`
    const call = { id: callId, name: step.tool, arguments: step.args ?? "{}" }
    push({ kind: "llm_completed", turn, content: "", tool_calls: [call], token_count: 40 })
    push({ kind: "tool_requested", turn, calls: [call] })
    if (step.denied) {
      push({
        kind: "tool_denied",
        turn,
        call_id: callId,
        tool_name: step.tool,
        reason: step.reason ?? `tool '${step.tool}' denied by rule '${step.tool}'`,
      })
    } else {
      const result = { call_id: callId, output: step.error_kind ? `{"error":"${step.error_kind}"}` : "ok" }
      if (step.error_kind) {
        result.is_error = true
        result.error_kind = step.error_kind
      }
      push({ kind: "tool_completed", turn, results: [result] })
    }
    turn++
  }

  push({ kind: "llm_completed", turn, content: profile.finalNote ?? "Done.", tool_calls: [], token_count: 50 })
  push({ kind: "run_terminal", reason: profile.termination, turns_used: profile.turns, total_tokens: profile.tokens })
  return events
}

/** @param {Task} task @returns {Verdict} */
function passVerdict(task) {
  return {
    passed: true,
    overallScore: 1,
    feedback: "meets all criteria",
    details: task.criteria.map(c => ({ criterion: c.text, passed: true, score: 1, feedback: "ok" })),
  }
}

/** @param {Task} task @param {string[]} failedIds @returns {Verdict} */
function failVerdict(task, failedIds) {
  const failed = new Set(failedIds)
  const details = task.criteria.map(c => ({
    criterion: c.text,
    passed: !failed.has(c.id),
    score: failed.has(c.id) ? 0 : 1,
    feedback: failed.has(c.id) ? "criterion not met" : "ok",
  }))
  return {
    passed: false,
    overallScore: details.reduce((s, d) => s + d.score, 0) / (details.length || 1),
    feedback: "one or more criteria unmet",
    details,
  }
}

// ── Adapter ───────────────────────────────────────────────────────────────────

/**
 * A deterministic TaskAdapter whose task outcomes depend only on the manifest.
 * @returns {TaskAdapter}
 */
export function createFixtureAdapter() {
  /** @type {Task[]} */
  const tasks = TASK_SPECS.map(({ id, goal, criteria, maxTurns }) => ({ id, goal, criteria, ...(maxTurns ? { maxTurns } : {}) }))
  const specById = new Map(TASK_SPECS.map(s => [s.id, s]))

  return {
    id: "fixture",
    listTasks: () => tasks.map(t => ({ ...t, criteria: t.criteria.map(c => ({ ...c })) })),
    /** @param {Task} task @param {any} manifest @returns {Promise<RunOutcome>} */
    async runTask(task, manifest) {
      const spec = specById.get(task.id)
      if (!spec) throw new Error(`fixture adapter: unknown task ${task.id}`)
      if (spec.decide(manifest)) {
        return {
          passed: true,
          verdict: passVerdict(spec),
          events: synthEvents(spec, { termination: "completed", turns: 1, tokens: 200, finalNote: "Task completed successfully." }),
          termination: "completed",
        }
      }
      const f = /** @type {FailureProfile} */ (spec.failure)
      return {
        passed: false,
        verdict: failVerdict(spec, f.failedCriteriaIds),
        events: synthEvents(spec, f),
        termination: f.termination,
      }
    },
  }
}

/**
 * A seed HarnessManifest matched to the fixture task set: `instructions.execution` mentions
 * "cite sources" (so `exec-cite` passes at the seed and a regression edit can break it), verification
 * is empty (so `verify-keyword` fails until addressed), and no nudges (so `nudge-guard` fails).
 * @returns {import("../../../node/src/harness/manifest.js").HarnessManifest}
 */
export function fixtureSeedManifest() {
  return {
    manifestVersion: 1,
    parent: null,
    modelProfile: "fixture-model",
    instructions: {
      execution: "Work step by step and cite sources for every factual claim.",
    },
    editableSurfaces: [...EDITABLE_SURFACES],
    audit: { round: 0, createdBy: "seed" },
  }
}

/** Default held-in / held-out split for the fixture e2e (ids only). */
export const FIXTURE_SPLITS = {
  heldIn: ["verify-keyword", "ceiling", "stable-1"],
  heldOut: ["exec-cite", "stable-2"],
}

/** CLI entry point — the fixture adapter ignores its context. */
export function createAdapter() {
  return createFixtureAdapter()
}
