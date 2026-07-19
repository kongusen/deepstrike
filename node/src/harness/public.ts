// `@deepstrike/sdk/harness` — one attempt engine with independent body/judge/carry/stop policies.
export {
  AttemptLoop,
  RuntimeAttemptBody,
  continueSession,
  freshWithFeedback,
  freshWithDigest,
} from "./harness.js"
export type {
  AttemptRequest, AttemptBodyContext, AttemptProgressEvent, AttemptBodyTerminal, AttemptBodyEvent,
  AttemptBody, PreparedAttempt, CarryPolicy, StopPolicy, AttemptOutcomeKind, AttemptOutcome,
  AttemptLoopEvent, AttemptLoopOptions, VerdictFn, Criterion, Verdict,
} from "./harness.js"
export { VerdictFnJudge, LlmEvalJudge, HybridJudge } from "./judge.js"
export type { AttemptJudge, JudgeContext, JudgeResult, SkillCandidate } from "./judge.js"
export { judge } from "../runtime/eval.js"
export type { VerdictDetail, JudgeArgs } from "../runtime/eval.js"

// Self-Harness H1: the harness face as data (manifest lineage + declarative event→note rules). The
// lab layer loads these through the compiled dist, so they live on this public barrel.
export {
  composeSystemPrompt,
  manifestDigest,
  applyManifest,
  applyPatch,
  validateManifest,
} from "./manifest.js"
export type {
  InstructionProfile,
  HarnessManifest,
  HarnessRuntimePatch,
  HarnessPatch,
} from "./manifest.js"
export { NudgeEngine, validateNudgeRules } from "./nudge.js"
export type { NudgeTrigger, NudgeRule, NudgeOutput } from "./nudge.js"
