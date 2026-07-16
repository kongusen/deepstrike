#!/usr/bin/env node
/**
 * Phase 6 SDK parity checker — static grep for OS mechanisms in each SDK tree.
 * Exit 0 when all required markers are present; 1 otherwise.
 */
import { readFileSync, existsSync } from "node:fs"
import { join } from "node:path"

const root = new URL("..", import.meta.url).pathname

const CHECKS = [
  {
    id: "node-memory-syscall",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["writeMemory", "queryMemory", "memory_validation_failed"],
  },
  {
    id: "python-memory-syscall",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["write_memory", "query_memory", "memory_validation_failed"],
  },
  {
    id: "core-memory-session-events",
    lang: "core",
    path: "crates/deepstrike-core/src/runtime/session.rs",
    patterns: ["MemoryValidationFailed", "MemoryRetrievalResult"],
  },
  {
    id: "node-os-profile",
    lang: "node",
    path: "node/src/runtime/os-profile.ts",
    patterns: ["osProfile", "assertNativeProfile"],
  },
  {
    id: "node-kernel-event-log",
    lang: "node",
    path: "node/src/runtime/kernel-event-log.ts",
    patterns: ["categoryForKind", "kernelObservationToSessionEvent"],
  },
  {
    // I4/K4 memory recall: run-start prefetch into decaying history + renewal re-query.
    // (The old applyKernelPageIn side-channel was retired with the PageInRequested observation.)
    id: "node-memory-recall",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["prefetchMemoryIntoHistory", 'action.kind === "execute_tool"'],
  },
  {
    // M2 资源配额 — Node is the reference: quotas flow into the kernel via the JSON event ABI.
    id: "node-resource-quota",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["resourceQuota", "set_resource_quota", "max_concurrent_subagents", "SchedulerPolicy"],
  },
  {
    id: "node-public-api-shape",
    lang: "node",
    path: "node/src/os/public.ts",
    patterns: ["MemoryWriteRateLimit", "ResourceQuota", "NativeOsProfile", "OsProfileId"],
  },
  {
    id: "python-resource-quota",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["resource_quota", "set_resource_quota", "max_concurrent_subagents", "SchedulerPolicy"],
  },
  {
    id: "python-public-api-shape",
    lang: "python",
    path: "python/deepstrike/__init__.py",
    patterns: ["MemoryWriteRateLimit", "ResourceQuota", "SchedulerPolicy", "OsProfile", "assert_native_profile"],
  },
  {
    id: "rust-resource-quota",
    lang: "rust",
    path: "rust/src/runtime/runner.rs",
    patterns: ["resource_quota", "SetResourceQuota", "ResourceQuota", "scheduler_policy"],
  },
  {
    id: "rust-public-api-shape",
    lang: "rust",
    path: "rust/src/lib.rs",
    patterns: ["MemoryWriteRateLimit", "ResourceQuota", "SchedulerPolicyConfig", "NativeOsProfile", "OsProfile", "assert_native_profile"],
  },
  {
    id: "wasm-resource-quota",
    lang: "wasm",
    path: "wasm/src/runtime/runner.ts",
    patterns: ["resourceQuota", "resource_quota", "max_concurrent_subagents"],
  },
  {
    id: "wasm-public-api-shape",
    lang: "wasm",
    path: "wasm/src/index.ts",
    patterns: ["MemoryWriteRateLimit", "ResourceQuota", "SchedulerPolicy", "NativeOsProfile", "OsProfileId"],
  },
  {
    id: "core-resource-quota",
    lang: "core",
    // kernel.rs split into kernel/{runtime,protocol}.rs; the event arm + setter live in runtime.rs.
    path: "crates/deepstrike-core/src/runtime/kernel/runtime.rs",
    patterns: ["SetResourceQuota", "set_resource_quota"],
  },
  {
    // Memory policy — Node is the reference: memory config flows into the kernel via the JSON
    // event ABI (`set_memory_policy`), the same channel as governance / scheduler / quota config.
    id: "node-memory-policy",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["memoryPolicy", "set_memory_policy", "retrieval_top_k", "max_content_bytes"],
  },
  {
    id: "python-memory-policy",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["memory_policy", "set_memory_policy", "retrieval_top_k", "max_content_bytes"],
  },
  {
    id: "rust-memory-policy",
    lang: "rust",
    path: "rust/src/runtime/runner.rs",
    patterns: ["memory_policy", "SetMemoryPolicy", "MemoryPolicy", "max_content_bytes"],
  },
  {
    id: "wasm-memory-policy",
    lang: "wasm",
    path: "wasm/src/runtime/runner.ts",
    patterns: ["memoryPolicy", "set_memory_policy", "retrieval_top_k", "max_content_bytes"],
  },
  {
    // Memory policy is kernel-enforced: the handler installs it via sm.set_memory_policy and the
    // WriteMemory / QueryMemory traps read it back.
    id: "core-memory-policy",
    lang: "core",
    // kernel.rs split into kernel/{runtime,protocol}.rs; the event arm + setter live in runtime.rs.
    path: "crates/deepstrike-core/src/runtime/kernel/runtime.rs",
    patterns: ["SetMemoryPolicy", "set_memory_policy", "memory_policy()"],
  },
  {
    id: "wasm-os-profile",
    lang: "wasm",
    path: "wasm/src/runtime/os-profile.ts",
    patterns: ["osProfile", "assertNativeProfile"],
  },
  {
    id: "wasm-kernel-event-log",
    lang: "wasm",
    path: "wasm/src/runtime/kernel-event-log.ts",
    patterns: ["categoryForKind", "kernelObservationToSessionEvent"],
  },
  {
    id: "wasm-runner-native",
    lang: "wasm",
    path: "wasm/src/runtime/runner.ts",
    patterns: ["osProfile", "assertNativeProfile", "kernelMaybeAction", "prefetchMemoryIntoHistory"],
  },
  {
    id: "python-os-profile",
    lang: "python",
    path: "python/deepstrike/runtime/os_profile.py",
    patterns: ["os_profile", "assert_native_profile"],
  },
  {
    id: "python-kernel-event-log",
    lang: "python",
    path: "python/deepstrike/runtime/kernel_event_log.py",
    patterns: ["category_for_kind", "kernel_observation_to_session_event"],
  },
  {
    id: "python-runner-native",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["os_profile", "assert_native_profile", "kernel_maybe_action", "_prefetch_memory_into_history"],
  },
  {
    id: "core-replay",
    lang: "core",
    path: "crates/deepstrike-core/src/runtime/replay.rs",
    patterns: ["rebuild_os_snapshot_from_events", "OsSnapshot"],
  },
  {
    id: "core-event-log",
    lang: "core",
    path: "crates/deepstrike-core/src/runtime/event_log.rs",
    patterns: ["KernelEventCategory", "category_for_kind"],
  },
  {
    id: "session-golden-spawn",
    lang: "fixtures",
    path: "tests/fixtures/session/os_snapshot_spawn_lifecycle.json",
    patterns: ["process_by_agent", "last_suspend"],
  },
  {
    id: "session-golden-ask-user",
    lang: "fixtures",
    path: "tests/fixtures/session/os_snapshot_ask_user.json",
    patterns: ["tool_gated_count", "ask_user"],
  },
  // ── dynamic workflow surface (W-audit): pinned as live invariants across the 3 driver SDKs. ──
  {
    // Index-faithful resume + W-1 signal replay + W-N3 submitter-aware batch drop.
    id: "core-workflow-resume",
    lang: "core",
    path: "crates/deepstrike-core/src/orchestration/workflow/run.rs",
    patterns: ["ResumedNodeOutcome", "resumed_placeholder_result", "parse_loop_iteration_id"],
  },
  {
    id: "core-workflow-submit-gate",
    lang: "core",
    path: "crates/deepstrike-core/src/scheduler/state_machine/workflow.rs",
    patterns: ["append_nodes_gated", "WorkflowNodesSubmitted", "submit_nodes_from"],
  },
  {
    id: "node-workflow-driver",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["resumed_outcomes", "recoverSubmittedWorkflowNodes", "dependencyOutputsNote", "buildWorkflowNodeCompletedEvent"],
  },
  {
    id: "node-workflow-loop-pace",
    lang: "node",
    path: "node/src/types/agent.ts",
    patterns: ["loopRound", "loop_round"],
  },
  {
    id: "node-loop-driver",
    lang: "node",
    path: "node/src/runtime/loop-driver.ts",
    patterns: ["foldLoopState", "signalAwareSleeper", "round_paced"],
  },
  {
    id: "python-workflow-driver",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["resumed_outcomes", "recover_submitted_workflow_nodes", "dependency_outputs_note"],
  },
  {
    id: "python-loop-driver",
    lang: "python",
    path: "python/deepstrike/runtime/loop_driver.py",
    patterns: ["fold_loop_state", "round_paced"],
  },
  {
    id: "wasm-workflow-driver",
    lang: "wasm",
    path: "wasm/src/runtime/runner.ts",
    patterns: ["resumed_outcomes", "recoverSubmittedWorkflowNodes", "dependencyOutputsNote"],
  },
  // wasm LoopDriver: EXPLICIT node+python-first decision (edge cron loops re-arm via wake_at_ms
  // from the host today); revisit when a wasm host needs in-process pacing.
  // ── multimodal attempt parity: AttemptLoop forwards attachments unconditionally; each driver
  // runner seeds them idempotently per session (dedupe against prior run_started records). ──
  {
    id: "node-attachment-seeding",
    lang: "node",
    path: "node/src/runtime/runner.ts",
    patterns: ["attachmentsAlreadySeeded"],
  },
  {
    id: "python-attachment-seeding",
    lang: "python",
    path: "python/deepstrike/runtime/runner.py",
    patterns: ["_attachments_already_seeded"],
  },
  {
    id: "wasm-attachment-seeding",
    lang: "wasm",
    path: "wasm/src/runtime/runner.ts",
    patterns: ["attachmentsAlreadySeeded"],
  },
]

let failed = 0
for (const check of CHECKS) {
  const file = join(root, check.path)
  if (!existsSync(file)) {
    console.error(`FAIL ${check.id}: missing file ${check.path}`)
    failed += 1
    continue
  }
  const text = readFileSync(file, "utf8")
  const missing = check.patterns.filter(p => !text.includes(p))
  if (missing.length) {
    console.error(`FAIL ${check.id} (${check.lang}): missing ${missing.join(", ")} in ${check.path}`)
    failed += 1
  } else {
    console.log(`OK   ${check.id} (${check.lang})`)
  }
}

if (failed > 0) {
  console.error(`\n${failed} parity check(s) failed`)
  process.exit(1)
}
console.log("\nSDK parity static checks passed")
