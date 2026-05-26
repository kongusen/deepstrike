# Implementation Plan: Agent OS Kernel Abstractions

## Overview

This plan turns the Claude Code architecture takeaways into DeepStrike kernel primitives. The goal is not to copy a CLI product surface, but to strengthen the pure Rust core so SDKs can expose richer agent operating-system behavior without each language inventing its own rules.

The first implementation slice is intentionally narrow: add typed capability and context-section registries in `deepstrike-core`, keep them pure and serializable, and connect them to the existing context manager where that can be done without changing SDK contracts.

## Architecture Decisions

- Capability awareness becomes a first-class kernel concept. Tools, skills, memory, knowledge, MCP servers, commands, and agents are represented in one `CapabilityManifest` so the model can be told what it can do from one stable source.
- Context rendering is split from context-section lifecycle policy. `ContextSectionRegistry` records cache policy, priority, token budget, and invalidation rules; renderer changes can then evolve without hard-coding every section.
- The kernel remains zero-I/O. SDKs still load files, call MCP, execute tools, and talk to providers; the kernel only stores metadata, sorts, filters, and emits schemas or render plans.
- Existing public behavior stays additive. Runtime v1 event schema and SDK APIs are not changed in the first slice.

## Phase 1: Foundation

### Task 1: Add `CapabilityManifest`

**Description:** Define a serializable manifest for model-visible capabilities and helper methods for deterministic tool-schema aggregation.

**Acceptance criteria:**
- Manifest can register user tools, skills, built-in meta-tools, MCP tools, commands, and agent capabilities.
- Tool schemas are emitted in deterministic order.
- Skill / memory / knowledge behavior remains compatible with the existing `ContextManager`.

**Verification:**
- `cargo test -p deepstrike-core capability`

**Files likely touched:**
- `crates/deepstrike-core/src/types/capability.rs`
- `crates/deepstrike-core/src/types/mod.rs`
- `crates/deepstrike-core/src/lib.rs`
- `crates/deepstrike-core/src/context/manager.rs`

### Task 2: Add `ContextSectionRegistry`

**Description:** Define typed context sections with cache and invalidation policy, independent of actual message storage.

**Acceptance criteria:**
- Sections have id, partition, priority, cache policy, invalidation policy, and optional token budget.
- Registry renders deterministic section plans ordered by priority then id.
- Registry can invalidate sections by event.

**Verification:**
- `cargo test -p deepstrike-core section`

**Files likely touched:**
- `crates/deepstrike-core/src/context/sections.rs`
- `crates/deepstrike-core/src/context/mod.rs`
- `crates/deepstrike-core/src/context/manager.rs`

### Checkpoint: Foundation

- `cargo test -p deepstrike-core capability section`
- No SDK API changes required.
- New types are additive and do not modify Runtime v1 event schema.

## Phase 2: Governance and Tool Lifecycle

### Task 3: Add `ToolDecisionPipeline` contract

**Description:** Generalize current governance into explicit stages that can model classifiers, hook decisions, permission gates, and post-observers while preserving the rule that permissive hooks cannot bypass deny rules.

**Acceptance criteria:**
- Pipeline verdict records the responsible stage.
- Deny decisions remain monotonic across layers.
- Existing `GovernancePipeline` can be adapted without breaking current tests.

**Verification:**
- Governance tests cover hook allow plus permission deny.

**Dependencies:** Phase 1.

## Phase 3: Agent Runtime Spec

### Task 4: Add `AgentRunSpec`

**Description:** Promote sub-agent role, isolation, inherited context, capability filters, and permission profile into a typed kernel contract.

**Acceptance criteria:**
- Explore, implement, verify, and plan roles are representable.
- Specs can derive a filtered `CapabilityManifest`.
- `VerificationContract` can be linked to a verify role.

**Verification:**
- Unit tests for capability filtering by agent role.

**Dependencies:** Task 1.

## Phase 4: Prompt Cache and Lifecycle Events

### Task 5: Add context snapshot cache hints

**Description:** Emit stable hashes for static system prefix and capability manifest so SDKs can preserve provider prompt-cache boundaries.

**Acceptance criteria:**
- Snapshot hashes are deterministic for equivalent section plans.
- Dynamic sections do not alter static prefix hash.

**Dependencies:** Task 2.

### Task 6: Draft Runtime v2 lifecycle event vocabulary

**Description:** Document additive event variants for agent lifecycle, permission decisions, capability changes, and cleanup completion.

**Acceptance criteria:**
- Runtime v1 remains frozen.
- Runtime v2 proposal maps each event to recovery or telemetry purpose.

**Dependencies:** Phase 1.

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Abstractions become too broad | High | First slice only stores metadata and deterministic ordering. |
| SDK bindings need immediate updates | Medium | Keep Phase 1 internal to `deepstrike-core` and additive. |
| Existing dirty worktree conflicts | Medium | Add new files where possible and make minimal edits to module exports. |
| Prompt cache semantics become provider-specific | Medium | Kernel emits generic cache policy and stable ordering; SDKs map to provider APIs. |

## Execution Scope for This Session

This session implements the kernel-facing portion of Phases 1-4:

- `CapabilityManifest`
- `ContextSectionRegistry`
- `ToolDecisionPipeline`
- `AgentRunSpec`
- `ContextSnapshotHint`
- Runtime v2 lifecycle event vocabulary draft
- focused Rust tests
