/**
 * Self-Harness evidence pipeline (S2) tests — Node built-in runner, no external deps.
 *
 * Run:  node --test benchmark/tests/selfharness-evidence.test.mjs
 *
 * Covers H2: extractFailureRecord field-exact values, deterministic failureSignature, exact-match
 * clustering (same mechanism ⇒ same cluster; different mechanism ⇒ distinct), a full clusters
 * golden, buildEvidenceBundle totals/ordering/median/passthrough, and renderExcerpt determinism +
 * bound + truncation marker. Fixtures are synthetic event dumps shaped exactly like bench
 * `*.events.json` (`{seq, event}[]`).
 */

import assert from "node:assert/strict"
import { test, describe } from "node:test"
import { readFileSync } from "node:fs"
import { fileURLToPath } from "node:url"
import path from "node:path"

import {
  extractFailureRecord,
  failureSignature,
  clusterFailures,
  buildEvidenceBundle,
} from "../selfharness/evidence.mjs"
import { renderExcerpt } from "../selfharness/trace-excerpt.mjs"

const FIX = path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures", "selfharness")
const loadEvents = name => JSON.parse(readFileSync(path.join(FIX, `${name}.events.json`), "utf8"))

// Shared verdict/criteria — the criteria carry stable contract ids the record maps back to.
const CRITERIA = [
  { id: "C_locate", text: "the agent locates the failing module", machineCheckable: true },
  { id: "C_report", text: "the agent reports findings as plain text" },
]
const FAIL_VERDICT = {
  passed: false,
  overallScore: 0.3,
  feedback: "did not locate the module",
  details: [
    { criterion: "the agent locates the failing module", passed: false, score: 0, feedback: "never read the right file" },
    { criterion: "the agent reports findings as plain text", passed: true, score: 1, feedback: "ok" },
  ],
}
const PASS_VERDICT = {
  passed: true,
  overallScore: 1,
  feedback: "solid diagnosis",
  details: [
    { criterion: "the agent locates the failing module", passed: true, score: 1, feedback: "ok" },
    { criterion: "the agent reports findings as plain text", passed: true, score: 1, feedback: "ok" },
  ],
}

const recordFor = (taskId, fixture, verdict = FAIL_VERDICT) =>
  extractFailureRecord({
    taskId,
    events: loadEvents(fixture),
    verdict,
    criteria: CRITERIA,
    eventsPath: `fixtures/selfharness/${fixture}.events.json`,
  })

// ── extractFailureRecord — field-exact ──────────────────────────────────────

describe("extractFailureRecord", () => {
  test("timeout + machineCheckable criterion fail + dense file_not_found → exact fields", () => {
    const r = recordFor("fnf-a", "timeout-fnf-a")
    assert.deepEqual(r, {
      taskId: "fnf-a",
      passed: false,
      termination: "timeout",
      failedCriteria: ["C_locate"],
      toolErrors: { file_not_found: 3 },
      toolUsage: { read_file: { calls: 3, errors: 3 } },
      denies: 0,
      entropyPeak: 0.71,
      turns: 4,
      totalTokens: 900,
      eventsPath: "fixtures/selfharness/timeout-fnf-a.events.json",
    })
  })

  test("no_progress + repeated denies → denies counted, no tool errors, null entropy", () => {
    const r = recordFor("deny-1", "no-progress-deny")
    assert.equal(r.termination, "no_progress")
    assert.equal(r.denies, 3)
    assert.deepEqual(r.toolErrors, {})
    assert.equal(r.entropyPeak, null)
    assert.equal(r.turns, 4)
    assert.equal(r.totalTokens, 700)
    assert.deepEqual(r.failedCriteria, ["C_locate"])
  })

  test("missing run_terminal → termination 'unknown', turns/tokens 0", () => {
    const r = recordFor("missing-1", "missing-terminal")
    assert.equal(r.termination, "unknown")
    assert.equal(r.turns, 0)
    assert.equal(r.totalTokens, 0)
    assert.deepEqual(r.toolErrors, { permission_denied: 1 })
  })

  test("passing run → passed true, no failed criteria", () => {
    const r = recordFor("pass-1", "passing", PASS_VERDICT)
    assert.equal(r.passed, true)
    assert.equal(r.termination, "completed")
    assert.deepEqual(r.failedCriteria, [])
    assert.equal(r.turns, 2)
    assert.equal(r.totalTokens, 400)
  })

  test("failedCriteria falls back to text ≤64 chars when no id matches", () => {
    const verdict = {
      passed: false,
      overallScore: 0,
      feedback: "x",
      details: [
        { criterion: "an unregistered criterion whose text is deliberately longer than sixty-four characters total", passed: false, score: 0, feedback: "no" },
      ],
    }
    const r = extractFailureRecord({ taskId: "t", events: loadEvents("timeout-fnf-a"), verdict, criteria: CRITERIA })
    assert.equal(r.failedCriteria.length, 1)
    assert.equal(r.failedCriteria[0].length, 64)
    assert.equal(r.failedCriteria[0], "an unregistered criterion whose text is deliberately longer than")
  })
})

// ── toolUsage — per-tool calls/errors, joined call_id → name (V2-S2) ─────────

describe("toolUsage extraction", () => {
  test("counts admitted tool_requested calls and joins tool_completed errors by call_id", () => {
    // fnf-a: three read_file calls, each errors → calls 3 / errors 3.
    assert.deepEqual(recordFor("fnf-a", "timeout-fnf-a").toolUsage, { read_file: { calls: 3, errors: 3 } })
  })

  test("multi-tool cluster: distinct tools counted separately, name-sorted", () => {
    // fnf-b: list_dir, read_file, run_tests each 1 call / 1 error; keys must be sorted.
    const u = recordFor("fnf-b", "timeout-fnf-b").toolUsage
    assert.deepEqual(Object.keys(u), ["list_dir", "read_file", "run_tests"]) // name-sorted
    assert.deepEqual(u, {
      list_dir: { calls: 1, errors: 1 },
      read_file: { calls: 1, errors: 1 },
      run_tests: { calls: 1, errors: 1 },
    })
  })

  test("denied-only run has empty toolUsage (denied calls never reach tool_requested)", () => {
    // no-progress-deny emits only llm_completed + tool_denied — no admitted calls.
    assert.deepEqual(recordFor("deny-1", "no-progress-deny").toolUsage, {})
  })

  test("a passing (non-error) call counts under calls with zero errors", () => {
    assert.deepEqual(recordFor("pass-1", "passing", PASS_VERDICT).toolUsage, { read_file: { calls: 1, errors: 0 } })
  })
})

// ── failureSignature — deterministic machine-fact axes ───────────────────────

describe("failureSignature", () => {
  test("cause = termination:sorted(criteria); symptom = dominant error kind", () => {
    assert.deepEqual(failureSignature(recordFor("fnf-a", "timeout-fnf-a")), {
      cause: "timeout:C_locate",
      symptom: "file_not_found",
    })
  })

  test("dominant error kind wins over a lower-count kind (fnf 2 > timeout 1)", () => {
    const r = recordFor("fnf-b", "timeout-fnf-b")
    assert.deepEqual(r.toolErrors, { file_not_found: 2, timeout: 1 })
    assert.equal(failureSignature(r).symptom, "file_not_found")
  })

  test("no tool errors but denies>0 → symptom 'denied'", () => {
    assert.equal(failureSignature(recordFor("deny-1", "no-progress-deny")).symptom, "denied")
  })

  test("clean run (no errors, no denies) → symptom 'clean'", () => {
    assert.equal(failureSignature(recordFor("pass-1", "passing", PASS_VERDICT)).symptom, "clean")
  })

  test("sorted criteria makes cause order-independent", () => {
    const base = recordFor("x", "timeout-fnf-a")
    const a = { ...base, failedCriteria: ["C_report", "C_locate"] }
    const b = { ...base, failedCriteria: ["C_locate", "C_report"] }
    assert.equal(failureSignature(a).cause, failureSignature(b).cause)
    assert.equal(failureSignature(a).cause, "timeout:C_locate,C_report")
  })
})

// ── clusterFailures — same mechanism clusters, different does not ─────────────

describe("clusterFailures", () => {
  test("two same-mechanism records land in one cluster; distinct mechanisms stay apart", () => {
    const clusters = clusterFailures([
      recordFor("fnf-a", "timeout-fnf-a"),
      recordFor("fnf-b", "timeout-fnf-b"),
      recordFor("deny-1", "no-progress-deny"),
      recordFor("missing-1", "missing-terminal"),
    ])
    assert.equal(clusters.length, 3)
    assert.equal(clusters[0].size, 2)
    assert.deepEqual(clusters[0].taskIds, ["fnf-a", "fnf-b"])
    // fnf-a and fnf-b share one signature; the other two are singletons.
    assert.ok(clusters.slice(1).every(c => c.size === 1))
  })

  test("clusters golden — full deterministic structure (size desc, then key asc)", () => {
    const clusters = clusterFailures([
      recordFor("fnf-a", "timeout-fnf-a"),
      recordFor("fnf-b", "timeout-fnf-b"),
      recordFor("deny-1", "no-progress-deny"),
      recordFor("missing-1", "missing-terminal"),
    ])
    assert.deepEqual(clusters, [
      {
        key: '{"cause":"timeout:C_locate","symptom":"file_not_found"}',
        signature: { cause: "timeout:C_locate", symptom: "file_not_found" },
        size: 2,
        taskIds: ["fnf-a", "fnf-b"],
      },
      {
        key: '{"cause":"no_progress:C_locate","symptom":"denied"}',
        signature: { cause: "no_progress:C_locate", symptom: "denied" },
        size: 1,
        taskIds: ["deny-1"],
      },
      {
        key: '{"cause":"unknown:C_locate","symptom":"permission_denied"}',
        signature: { cause: "unknown:C_locate", symptom: "permission_denied" },
        size: 1,
        taskIds: ["missing-1"],
      },
    ])
  })
})

// ── buildEvidenceBundle — totals / ordering / median / passthrough ───────────

describe("buildEvidenceBundle", () => {
  const previousAttempts = [
    { surface: "instructions.verification", summary: "add a run-tests checklist", accepted: true, deltaIn: 0.2, deltaHo: 0.1 },
  ]
  // 4 failing records + 3 passing (turns 2/4/6, tokens 400/600/800 → medians 4 / 600).
  const passExtra = [
    { taskId: "pass-2", passed: true, termination: "completed", failedCriteria: [], toolErrors: {}, denies: 0, entropyPeak: null, turns: 4, totalTokens: 600, eventsPath: "" },
    { taskId: "pass-3", passed: true, termination: "completed", failedCriteria: [], toolErrors: {}, denies: 0, entropyPeak: null, turns: 6, totalTokens: 800, eventsPath: "" },
  ]
  const records = [
    recordFor("fnf-a", "timeout-fnf-a"),
    recordFor("fnf-b", "timeout-fnf-b"),
    recordFor("deny-1", "no-progress-deny"),
    recordFor("missing-1", "missing-terminal"),
    recordFor("pass-1", "passing", PASS_VERDICT),
    ...passExtra,
  ]
  const bundle = buildEvidenceBundle({
    round: 2,
    harnessDigest: "abc123",
    records,
    previousAttempts,
    eventsByTask: {
      "fnf-a": loadEvents("timeout-fnf-a"),
      "fnf-b": loadEvents("timeout-fnf-b"),
    },
  })

  test("totals count all tasks split pass/fail", () => {
    assert.deepEqual(bundle.totals, { tasks: 7, passed: 3, failed: 4 })
  })

  test("clusters sorted size desc; failing tasks only", () => {
    assert.equal(bundle.clusters.length, 3)
    assert.equal(bundle.clusters[0].size, 2)
    assert.deepEqual(bundle.clusters[0].taskIds, ["fnf-a", "fnf-b"])
  })

  test("representative excerpts attached (≤2) when events available", () => {
    const c0 = bundle.clusters[0]
    assert.equal(c0.excerpt.length, 2)
    assert.deepEqual(c0.excerpt.map(e => e.taskId), ["fnf-a", "fnf-b"])
    assert.ok(c0.excerpt.every(e => typeof e.text === "string" && e.text.length > 0))
    // clusters without supplied events get an empty excerpt list.
    assert.deepEqual(bundle.clusters[1].excerpt, [])
  })

  test("cluster toolUsage aggregate sums member records, name-sorted (V2-S2)", () => {
    // cluster[0] = {fnf-a, fnf-b}: read_file 3+1 calls / 3+1 errors, plus fnf-b's list_dir + run_tests.
    assert.deepEqual(bundle.clusters[0].toolUsage, {
      list_dir: { calls: 1, errors: 1 },
      read_file: { calls: 4, errors: 4 },
      run_tests: { calls: 1, errors: 1 },
    })
    // singleton denied cluster has no admitted tool calls.
    const denyCluster = bundle.clusters.find(c => c.taskIds.includes("deny-1"))
    assert.deepEqual(denyCluster.toolUsage, {})
  })

  test("passingNote medians correct", () => {
    assert.deepEqual(bundle.passingNote, { count: 3, medianTurns: 4, medianTokens: 600 })
  })

  test("previousAttempts passed through unchanged", () => {
    assert.deepEqual(bundle.previousAttempts, previousAttempts)
    assert.equal(bundle.round, 2)
    assert.equal(bundle.harnessDigest, "abc123")
  })

  test("passingNote medians are null when no passing runs", () => {
    const b = buildEvidenceBundle({ round: 0, harnessDigest: "d", records: [recordFor("fnf-a", "timeout-fnf-a")] })
    assert.deepEqual(b.passingNote, { count: 0, medianTurns: null, medianTokens: null })
  })
})

// ── scope isolation (V2-S1) — stamp + mixed-scope guard ──────────────────────

describe("scope isolation", () => {
  const scoped = (taskId, scope) =>
    extractFailureRecord({ taskId, events: loadEvents("timeout-fnf-a"), verdict: FAIL_VERDICT, criteria: CRITERIA, scope })

  test("extractFailureRecord stamps scope only when supplied (absent ⇒ no key)", () => {
    assert.equal(Object.hasOwn(scoped("t", undefined), "scope"), false)
    assert.equal(scoped("t", "alice").scope, "alice")
  })

  test("bundle stamps its scope; matching records pass", () => {
    const bundle = buildEvidenceBundle({ round: 0, harnessDigest: "d", scope: "alice", records: [scoped("a", "alice")] })
    assert.equal(bundle.scope, "alice")
    assert.equal(bundle.totals.tasks, 1)
  })

  test("absent record scope ≡ 'default' bundle scope (both normalize to default)", () => {
    // Bundle with no scope, records with no scope: normalize to "default" on both sides ⇒ no throw.
    const bundle = buildEvidenceBundle({ round: 0, harnessDigest: "d", records: [recordFor("fnf-a", "timeout-fnf-a")] })
    assert.equal(bundle.scope, "default")
    // Explicit "default" bundle scope also matches an absent-scope record.
    const explicit = buildEvidenceBundle({ round: 0, harnessDigest: "d", scope: "default", records: [recordFor("fnf-a", "timeout-fnf-a")] })
    assert.equal(explicit.totals.tasks, 1)
  })

  test("a foreign-scope record THROWS, naming both scopes (never silently filtered)", () => {
    assert.throws(
      () => buildEvidenceBundle({ round: 0, harnessDigest: "d", scope: "alice", records: [scoped("a", "alice"), scoped("b", "bob")] }),
      /scope "bob".*bundle scope is "alice"|"alice".*"bob"/,
    )
    // A record retains ALL its members — a scope mismatch is a fault, not a subset to keep.
    assert.throws(
      () => buildEvidenceBundle({ round: 0, harnessDigest: "d", records: [scoped("a", "alice")] }), // bundle "default" vs record "alice"
      /refusing to mix scopes/,
    )
  })
})

// ── renderExcerpt — determinism, bound, truncation marker ─────────────────────

describe("renderExcerpt", () => {
  const events = loadEvents("timeout-fnf-a")

  test("deterministic — two renders are byte-identical", () => {
    assert.equal(renderExcerpt(events), renderExcerpt(events))
  })

  test("renders llm turns, tool ERROR lines, and terminal line under the default bound", () => {
    const out = renderExcerpt(events)
    assert.ok(out.length <= 4000)
    assert.match(out, /T0 S: read_file/)
    assert.match(out, /T0 tool read_file\(\{"path":"src\/parser\.js"\}\) -> ERROR\[file_not_found\]/)
    assert.match(out, /END timeout/)
  })

  test("tool_denied renders a DENIED line", () => {
    const out = renderExcerpt(loadEvents("no-progress-deny"))
    assert.match(out, /DENIED write_file: tool 'write_file' denied by rule 'write_file'/)
    assert.match(out, /END no_progress/)
  })

  test("missing run_terminal → END unknown", () => {
    assert.match(renderExcerpt(loadEvents("missing-terminal")), /END unknown/)
  })

  test("exceeding maxChars → bounded output with an omission marker", () => {
    const out = renderExcerpt(events, { maxChars: 150 })
    assert.ok(out.length <= 150, `length ${out.length} exceeds bound`)
    assert.match(out, /…\[\d+ steps omitted\]…/)
    // head and tail are both preserved.
    assert.ok(out.startsWith("T0 S: read_file"))
    assert.ok(out.endsWith("END timeout"))
  })

  test("truncation is deterministic too", () => {
    assert.equal(renderExcerpt(events, { maxChars: 150 }), renderExcerpt(events, { maxChars: 150 }))
  })
})
