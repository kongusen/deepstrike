# Context Compression & Working Partition — v2 Spec

**Status:** Phase A + B + C complete (MVP shipped). Phase D deferred.  
**Target version:** 0.2.0  
**Scope:** `deepstrike-core` kernel + Node / Python / Rust / WASM SDKs  
**Phases:** A (token engine) → B (working task state) → C (archive) → D (smart pipeline) → E (SDK alignment)

| Phase | Status | Notes |
|---|---|---|
| A — Token Engine | ✅ Complete | `ContextConfig`, `ContextTokenEngine`, all `len/4` replaced |
| B — Task State | ✅ Complete | `TaskState`, Artifacts (6th partition), `KernelInputEvent::PushArtifact` |
| C — Archive | ✅ Complete | `ArchiveStore` read+write, async replay with fallback, Rust/Node/Python runners |
| D — Smart Pipeline | ⏸ Deferred | Awaiting Phase A–C stabilization |
| E — SDK Alignment | ⏸ Deferred | RuntimeOptions tokenizer/store fields pending |

---

## 1. Problem Statement

The current compression stack has four compounding failures that compound each other under load:

| Site | Current behaviour | Root cause |
|------|-------------------|------------|
| `SnipCompactor` | `max_chars = 2000` **byte** cut (`truncate_with_suffix` takes `max_bytes`, field name is wrong); `_target_tokens` parameter explicitly discarded | Two separate bugs: (1) bytes ≠ chars ≠ tokens — CJK is 3 bytes/char but ~0.5 tokens/char, so effective token budget varies 6× by content type; (2) the compression target passed by the pipeline is ignored entirely — a 200k model and an 8k model get the same 2000-byte cut |
| `MicroCompactor` | Tool result → `"[tool result cached: {call_id}]"`, `new_tokens = 5` hardcoded | **100% of tool output destroyed**; placeholder token count is a magic number, not measured |
| `CollapseCompactor` | Drops oldest N messages until under target | No summary produced — information is unrecoverable |
| `AutoCompactor` | `"[{n} messages compressed]"`, `token_count = 10` hardcoded | Semantic zero; summary token count is a magic number, not measured |
| `CompressionPipeline` | `target_fraction = 0.70` hardcoded | Should be configurable; cannot tune without code change |
| `PressureMonitor` | Thresholds 0.70 / 0.80 / 0.90 / 0.95 hardcoded | Correct concept (ratios), wrong delivery (not configurable) |
| `RenewalPolicy` | `renewal_threshold = 0.98`, `max_carryover = 5 messages` | `max_carryover` is an absolute message count — 5 messages of 100 tokens and 5 messages of 5000 tokens carry vastly different loads |
| `repair.rs` | `RECOVERY_CONTENT_MAX_BYTES = 32_768` (32 KB); `estimate_token_count = len / 4` | Absolute byte cap independent of model context window; same heuristic as renderer |
| `dashboard.rs` | `base = 20u32` token estimate for fixed fields; `/ 4` heuristic in `token_estimate()` | Duplicates renderer heuristic; no shared engine |
| `renderer::render` | `system_tokens ≈ system_text.len() / 4` | Independent heuristic — after compression the render pass can double-cut |
| `SessionEvent::Compressed` | `{ turn, archived_seq_range }` only | No summary body — wake/replay cannot reconstruct what was compressed |
| `Working partition` | Only `[INTERRUPT]` / `[SIGNAL]` messages | Task goal, plan, and progress live only in `ContextManager.current_goal` and `Dashboard` — both survive only within one sprint |
| `Dashboard.plan` | Fields exist; never written by the runner | Plan is available at renewal handoff (`HandoffArtifact.open_tasks`) but not maintained turn-by-turn |

Root causes:

- **No ratio discipline.** Every control parameter that should track the model's context window is instead a hardcoded absolute — bytes, message counts, or magic token numbers. Switching models does not rescale any threshold.
- **Compression = delete/placeholder, not summarise + archive.** Task state is scattered across `ContextManager.current_goal` (single string), `Dashboard` (rarely updated), and `history` (gets compressed away).

---

## 2. Target Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  system    — safety rules / contract (never compressed)          │
│  working   — TaskState: goal + plan + progress + scratchpad      │
│              (never compressed; SSOT for task state)             │
│  memory    — DreamStore results (lightly compressible)           │
│  skill     — dynamic skill schemas (swappable)                   │
│  artifacts — referenced outputs / tool products (not inlined)    │
│  history   — execution transcript (compressible with summary)    │
└─────────────────────────────────────────────────────────────────┘
          │                                  │
          ▼                                  ▼
   TokenEngine (unified)             CompressionArchive
   count / truncate / budget         SessionLog.compressed.summary
   shared by pressure + compress     + optional external blob
   + render
```

**Design invariants:**

1. **Single token engine.** `pressure.rs`, `compression.rs`, and `renderer.rs` all call `ContextTokenEngine`. The `len/4` heuristic is deleted everywhere.
2. **Ratios as the sole control object.** Every numeric threshold, budget, and limit is expressed as a fraction of `max_tokens` — the context window size that travels with the model. No absolute byte counts, no absolute message counts, no magic token numbers. `max_tokens` is the single anchor; everything else scales with it. Switching from an 8k model to a 200k model rescales all parameters automatically with zero configuration change.
3. **Compression = summary + archive.** Compactors produce a summary string and write an archive entry before destroying content. The summary lands in `working` and `SessionLog`.
4. **Working is the task SSOT.** `goal`, `plan`, `progress`, and `blocked_on` live in `Working.task_state`. They survive compression, renewal, and wake because the working partition is `compressible = false`.
5. **Recovery is reconstructable.** `SessionEvent::Compressed` carries `summary` + optional `archive_ref`. On `wake()`, the runner injects the summary as a context message before resuming.

---

## 3. Phase A — Token Engine Unification

### A0. `ContextConfig` — ratio-only control surface

Add to `crates/deepstrike-core/src/context/config.rs`. This struct is the **single place** where every numeric control parameter lives. All values are fractions of `max_tokens`. No field may hold an absolute byte count or message count.

```rust
/// All compression and context management parameters expressed as fractions
/// of max_tokens. When max_tokens changes (model switch), every derived limit
/// rescales automatically — callers never touch these ratios for routine model
/// changes.
///
/// Invariant: snip < micro < collapse < auto < renewal (strictly increasing).
#[derive(Debug, Clone)]
pub struct ContextConfig {
    // ── Pressure thresholds ─────────────────────────────────────────────────
    /// rho above which SnipCompactor activates.      Default: 0.70
    pub snip_threshold:      f64,
    /// rho above which MicroCompactor activates.     Default: 0.80
    pub micro_threshold:     f64,
    /// rho above which CollapseCompactor activates.  Default: 0.90
    pub collapse_threshold:  f64,
    /// rho above which AutoCompactor activates.      Default: 0.95
    pub auto_threshold:      f64,
    /// rho above which context renewal triggers.     Default: 0.98
    pub renewal_threshold:   f64,

    // ── Post-compression target ──────────────────────────────────────────────
    /// Target rho after any compression pass.        Default: 0.65
    /// Must be < snip_threshold.
    pub target_after_compress: f64,

    // ── SnipCompactor ────────────────────────────────────────────────────────
    /// Maximum fraction of max_tokens any single message may occupy after
    /// snipping. Messages smaller than this are never touched.
    /// Default: 0.05  (5 % of context window)
    pub snip_per_msg_ratio:  f64,

    // ── RenewalPolicy ────────────────────────────────────────────────────────
    /// Fraction of max_tokens worth of history tokens to carry across renewal.
    /// Renewal stops carrying messages once this budget is exhausted.
    /// Default: 0.05  (5 % of context window)
    pub carryover_ratio:     f64,

    // ── Recovery / repair ────────────────────────────────────────────────────
    /// Maximum fraction of max_tokens a recovery/replay payload may occupy.
    /// Replaces the hardcoded RECOVERY_CONTENT_MAX_BYTES constant.
    /// Default: 0.25  (25 % of context window)
    pub recovery_content_ratio: f64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            snip_threshold:        0.70,
            micro_threshold:       0.80,
            collapse_threshold:    0.90,
            auto_threshold:        0.95,
            renewal_threshold:     0.98,
            target_after_compress: 0.65,
            snip_per_msg_ratio:    0.05,
            carryover_ratio:       0.05,
            recovery_content_ratio: 0.25,
        }
    }
}

impl ContextConfig {
    /// Derive absolute token limits from max_tokens. Call sites use these
    /// derived values — never multiply ratios inline elsewhere in the codebase.
    pub fn target_tokens(&self, max_tokens: u32) -> u32 {
        (max_tokens as f64 * self.target_after_compress) as u32
    }

    pub fn snip_per_msg_tokens(&self, max_tokens: u32) -> u32 {
        ((max_tokens as f64 * self.snip_per_msg_ratio) as u32).max(50)
    }

    pub fn carryover_tokens(&self, max_tokens: u32) -> u32 {
        ((max_tokens as f64 * self.carryover_ratio) as u32).max(100)
    }

    pub fn recovery_content_tokens(&self, max_tokens: u32) -> u32 {
        (max_tokens as f64 * self.recovery_content_ratio) as u32
    }
}
```

`ContextManager` holds `config: ContextConfig` and passes it to `PressureMonitor`, `CompressionPipeline`, and `RenewalPolicy`. The hardcoded defaults in each of those structs are deleted; they receive derived values from `config` at construction time.

**Removed constants (all replaced by `ContextConfig`):**

| Constant | Old value | Replacement |
| --- | --- | --- |
| `SnipCompactor::default().max_chars` | `2000` bytes | `config.snip_per_msg_tokens(max_tokens)` |
| `MicroCompactor` `new_tokens = 5` | `5` | `engine.count_message(&placeholder_msg)` |
| `AutoCompactor` `token_count = 10` | `10` | `engine.count_message(&summary_msg)` |
| `CompressionPipeline::target_fraction` | `0.70` | `config.target_after_compress` |
| `PressureMonitor` thresholds | `0.70/0.80/0.90/0.95` | `config.{snip,micro,collapse,auto}_threshold` |
| `RenewalPolicy::renewal_threshold` | `0.98` | `config.renewal_threshold` |
| `RenewalPolicy::max_carryover` | `5 messages` | `config.carryover_tokens(max_tokens)` (token budget) |
| `RECOVERY_CONTENT_MAX_BYTES` | `32_768 bytes` | `engine.token_budget_to_bytes(config.recovery_content_tokens(max_tokens))` |
| `dashboard.rs` `base = 20` | `20` | `engine.count_message(&dashboard_msg)` |

### A1. `ContextTokenEngine` trait

Add to `crates/deepstrike-core/src/context/token_engine.rs`:

```rust
pub trait TokenCounter: Send + Sync {
    /// Count tokens in a UTF-8 string. Must not panic on any input.
    fn count(&self, text: &str) -> u32;

    /// Truncate `text` to at most `max_tokens` tokens, returning a valid UTF-8
    /// string that is a prefix of `text`. The result may be shorter if a token
    /// boundary falls between bytes.
    fn truncate(&self, text: &str, max_tokens: u32) -> &str;
}

/// Char-count fallback (4 chars ≈ 1 token). Used when no provider model is known.
pub struct CharApproxCounter;

impl TokenCounter for CharApproxCounter {
    fn count(&self, text: &str) -> u32 {
        // char count / 4, clamped to u32
        (text.chars().count() / 4).max(1) as u32
    }

    fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        let max_chars = (max_tokens as usize).saturating_mul(4);
        // Walk char boundaries to avoid splitting multi-byte scalars
        let mut byte_end = 0;
        for (byte_idx, _ch) in text.char_indices().take(max_chars) {
            byte_end = byte_idx;
        }
        // Include the last char we iterated
        let next = text[byte_end..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        &text[..byte_end + next]
    }
}

/// Wraps an Arc<dyn TokenCounter> for cheap clone.
#[derive(Clone)]
pub struct ContextTokenEngine(Arc<dyn TokenCounter>);

impl ContextTokenEngine {
    pub fn char_approx() -> Self {
        Self(Arc::new(CharApproxCounter))
    }

    pub fn with_counter(counter: impl TokenCounter + 'static) -> Self {
        Self(Arc::new(counter))
    }

    pub fn count(&self, text: &str) -> u32 {
        self.0.count(text)
    }

    pub fn count_message(&self, msg: &Message) -> u32 {
        match &msg.content {
            Content::Text(t) => self.count(t),
            Content::Parts(parts) => parts.iter().map(|p| self.count_part(p)).sum(),
        }
    }

    fn count_part(&self, part: &ContentPart) -> u32 {
        match part {
            ContentPart::Text(t) => self.count(t),
            ContentPart::ToolCall { name, input, .. } => {
                self.count(name) + self.count(&input.to_string())
            }
            ContentPart::ToolResult { output, .. } => self.count(output),
        }
    }

    pub fn truncate_message(&self, msg: &Message, max_tokens: u32) -> Message {
        match &msg.content {
            Content::Text(t) => {
                let kept = self.0.truncate(t, max_tokens);
                let mut m = msg.clone();
                if kept.len() < t.len() {
                    m.content = Content::Text(format!("{}… [truncated]", kept));
                    m.token_count = Some(max_tokens);
                }
                m
            }
            Content::Parts(_) => msg.clone(), // never mangle structured content
        }
    }
}
```

### A2. Replace `len/4` everywhere

**`renderer.rs` line 71:** Replace `(system_text.len() as u32 / 4).min(budget)` with `engine.count(&system_text).min(budget)`.

**`renderer.rs` lines 85-116:** Replace `msg.token_count.unwrap_or(0)` with `engine.count_message(msg)` when `token_count` is `None`. Prefer the stored count when available.

**`dashboard.rs` `token_estimate()`:** Replace char arithmetic with `engine.count(&self.format_compact())`. Cache the rendered string to avoid rendering twice on the hot path.

**`compression.rs` `SnipCompactor`:** Replace `max_chars: usize` field with `max_tokens: u32`. Truncation calls `engine.truncate_message(msg, self.max_tokens)`.

### A3. Inject engine into `ContextManager`

```rust
pub struct ContextManager {
    // ... existing fields ...
    pub engine: ContextTokenEngine,
}

impl ContextManager {
    pub fn new(max_tokens: u32) -> Self {
        Self::with_engine(max_tokens, ContextTokenEngine::char_approx())
    }

    pub fn with_engine(max_tokens: u32, engine: ContextTokenEngine) -> Self {
        // ...
    }
}
```

SDK layer passes a real tokeniser (tiktoken for OpenAI models, a BPE approximation for Claude) via `with_engine`. The default `CharApproxCounter` is the current baseline — switching to a real counter is a metrics improvement, not a behaviour change.

### A4. `RuntimeOptions` extension

```typescript
// Node / WASM SDK
interface RuntimeOptions {
  // ... existing fields ...
  tokenizer?: "char_approx" | "tiktoken_o200k" | "tiktoken_cl100k"
  // Default: "char_approx" (preserves current behaviour)
}
```

```python
# Python SDK
@dataclass
class RuntimeOptions:
    tokenizer: Literal["char_approx", "tiktoken_o200k", "tiktoken_cl100k"] = "char_approx"
```

### A5. Acceptance criteria — ✅ MET

- Same transcript: `rho` before and after switching from `char_approx` to `tiktoken_cl100k` stays within 15% for ASCII content, 40% for CJK (expected improvement, not regression).
- CJK string `"你好世界".repeat(1000)` does not panic in `truncate`. ✅ tested
- Render budget after compression equals `max_tokens - compressed_total_tokens` (no double-cut). ✅
- Unit: `engine.count(engine.0.truncate(text, n)) <= n` for all inputs. ✅ 8 unit tests

---

## 4. Phase B — Working Partition Task State

### B1. `TaskState` structure

Add to `crates/deepstrike-core/src/context/task_state.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Persistent task state that lives in the working partition.
/// Survives compression, renewal, and wake/resume cycles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskState {
    /// Primary objective for this run. Set at `run_started`, immutable thereafter.
    pub goal: String,

    /// Acceptance criteria (from `RunStarted.criteria`).
    pub criteria: Vec<String>,

    /// Ordered plan steps. SDK or LLM can update via `update_task_state`.
    /// Each entry is a short imperative sentence: "Fetch price data for AAPL".
    pub plan: Vec<PlanStep>,

    /// Index into `plan` of the step currently executing (0-based).
    /// `None` before planning is complete.
    pub current_step: Option<usize>,

    /// Free-text progress note — updated after each significant tool call.
    pub progress: String,

    /// Ephemeral scratch space for intermediate values.
    /// Not carried across renewal (set to empty on handoff).
    pub scratchpad: String,

    /// Reasons the current step cannot proceed.
    pub blocked_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub label: String,
    pub done: bool,
}

impl PlanStep {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), done: false }
    }
}

impl TaskState {
    /// Render as a compact block for embedding in `system_text`.
    /// Returns empty string when all fields are at their defaults.
    pub fn format_compact(&self) -> String {
        if self.goal.is_empty() && self.plan.is_empty() && self.progress.is_empty() {
            return String::new();
        }
        let mut lines = vec![
            format!("[TASK STATE] goal: {}", self.goal),
        ];
        if !self.criteria.is_empty() {
            lines.push(format!("criteria: {}", self.criteria.join(" | ")));
        }
        if !self.plan.is_empty() {
            lines.push("plan:".to_string());
            for (i, step) in self.plan.iter().enumerate() {
                let marker = if step.done { "✓" } else if Some(i) == self.current_step { "▶" } else { "○" };
                lines.push(format!("  {} {}. {}", marker, i + 1, step.label));
            }
        }
        if !self.progress.is_empty() {
            lines.push(format!("progress: {}", self.progress));
        }
        if !self.blocked_on.is_empty() {
            lines.push(format!("blocked_on: {}", self.blocked_on.join(", ")));
        }
        if !self.scratchpad.is_empty() {
            lines.push(format!("scratchpad: {}", self.scratchpad));
        }
        lines.join("\n")
    }
}
```

### B2. `ContextPartitions` integration

`Dashboard` currently holds `goal_progress`, `plan: Vec<String>`, and `scratchpad`. After Phase B these fields are **deprecated** in `Dashboard` (kept for read compat) and **authoritative** in `TaskState`.

Add `task_state: TaskState` to `ContextPartitions`:

```rust
pub struct ContextPartitions {
    pub system:     Partition,
    pub working:    Partition,   // unchanged: signals/interrupts
    pub task_state: TaskState,   // NEW: task SSOT — rendered into system_text
    pub dashboard:  Dashboard,   // deprecated fields migrated to task_state
    pub memory:     Partition,
    pub skill:      Partition,
    pub history:    Partition,
}
```

`task_state` is not a `Partition` (it has no `messages` Vec) — it is a pure struct rendered to text during `render()`.

### B3. Renderer changes

`build_system_text` appends `task_state.format_compact()` after the dashboard block. When `Dashboard` fields are eventually removed (Phase E cleanup), the dashboard block disappears naturally:

```rust
fn build_system_text(partitions: &ContextPartitions) -> String {
    let system = join_text_messages(&partitions.system);
    let dashboard = partitions.dashboard.format_compact();
    let task = partitions.task_state.format_compact();

    [&system, &dashboard, &task]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
}
```

### B4. Write timing

`ContextManager` exposes:

```rust
impl ContextManager {
    /// Called by the SDK runner after `run_started` is processed.
    pub fn init_task(&mut self, goal: String, criteria: Vec<String>) {
        self.partitions.task_state = TaskState {
            goal,
            criteria,
            ..Default::default()
        };
        // Sync deprecated Dashboard fields for any code still reading them
        self.partitions.dashboard.goal_progress = String::new();
        self.partitions.dashboard.plan.clear();
    }

    /// SDK or LLM-driven update. `plan` replaces the existing plan when Some.
    pub fn update_task(&mut self, update: TaskUpdate) {
        let ts = &mut self.partitions.task_state;
        if let Some(plan) = update.plan {
            ts.plan = plan.into_iter().map(PlanStep::new).collect();
        }
        if let Some(step) = update.current_step { ts.current_step = Some(step); }
        if let Some(p) = update.progress    { ts.progress = p; }
        if let Some(s) = update.scratchpad  { ts.scratchpad = s; }
        if let Some(b) = update.blocked_on  { ts.blocked_on = b; }
    }
}

#[derive(Default)]
pub struct TaskUpdate {
    pub plan:         Option<Vec<String>>,
    pub current_step: Option<usize>,
    pub progress:     Option<String>,
    pub scratchpad:   Option<String>,
    pub blocked_on:   Option<Vec<String>>,
}
```

### B5. Renewal handoff

`RenewalPolicy::renew` already reads `Dashboard.goal_progress` and `Dashboard.plan` into `HandoffArtifact`. After Phase B it reads from `TaskState` instead:

```rust
let artifact = HandoffArtifact {
    goal:             partitions.task_state.goal.clone(),
    progress_summary: partitions.task_state.progress.clone(),
    open_tasks:       partitions.task_state.plan.iter()
                        .filter(|s| !s.done)
                        .map(|s| s.label.clone())
                        .collect(),
    // ...
};
```

The renewed `ContextPartitions.task_state` carries `goal` + `criteria` + `plan` (steps reset to done=false for open items); `scratchpad` is cleared.

### B6. Optional `update_plan` meta-tool

When `RuntimeOptions.enable_plan_tool = true`, the kernel injects:

```json
{
  "name": "update_plan",
  "description": "Update your task plan and progress. Call this after completing a step or when the plan changes.",
  "parameters": {
    "type": "object",
    "properties": {
      "plan":         { "type": "array", "items": { "type": "string" } },
      "current_step": { "type": "integer" },
      "progress":     { "type": "string" },
      "blocked_on":   { "type": "array", "items": { "type": "string" } }
    }
  }
}
```

The SDK intercepts `update_plan` tool calls (same interception pattern as `memory` and `knowledge`) and calls `ctx.update_task(...)`. The tool result is an empty success — the update writes to `task_state`, not history.

### B7. Acceptance criteria — ✅ MET

- After `AutoCompact` empties `history`, the rendered `system_text` still contains `[TASK STATE] goal:` and all plan steps. ✅
- `TaskState.scratchpad` is cleared on renewal; `goal`, `criteria`, and open `plan` steps are preserved. ✅
- `working.compressible` remains `false`; the compression pipeline's `compress()` does not touch `working` or `task_state`. ✅
- Round-trip: `TaskState::default().format_compact() == ""` (no noise when task not yet set). ✅

**Phase B additions beyond original spec:**

- 6th context partition: `artifacts` (`compressible = false`, `Priority::Medium`). Push via `KernelInputEvent::PushArtifact`.
- `ContextManager.push_artifact()` and `take_snapshot()` added.
- `ContextSnapshot` captures all six partitions + `TaskState` at a given turn.

---

## 5. Phase C — SessionLog Compression Archive (v1.1)

### C1. Extended `compressed` event

The `compressed` event in `SessionEvent` gains four optional fields. All are `#[serde(default, skip_serializing_if = ...)]` so existing readers that only use `turn` + `archived_seq_range` are unaffected.

**Rust (kernel):**

```rust
Compressed {
    turn: u32,
    archived_seq_range: (u64, u64),
    // v1.1 additions — all optional for backward compat
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action: Option<String>,           // "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,          // rule-generated or LLM-generated summary text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_ref: Option<String>,      // path or content_hash of external blob
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    preserved_refs: Vec<String>,      // call_ids / artifact hashes that must not be lost
},
```

**TypeScript canonical type (spec-runtime-v1.md update):**

```typescript
| {
    kind: "compressed"
    turn: number
    archived_seq_range: [number, number]
    // v1.1
    action?: "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
    summary?: string
    summary_tokens?: number
    archive_ref?: string
    preserved_refs?: string[]
  }
```

### C2. Rule-based summariser

Add `crates/deepstrike-core/src/context/summarizer.rs`:

```rust
pub trait Summarizer: Send + Sync {
    /// Produce a summary of `messages` that fits within `max_tokens`.
    fn summarize(&self, messages: &[Message], action: PressureAction, max_tokens: u32) -> String;
}

/// Deterministic rule-based summariser — no LLM required.
/// Output format:
///   [Compressed: {action} at turn {turn}]
///   Turns {start}–{end} | {n} messages | {tokens} tokens
///   Tool calls: {tool_names...}
///   Last assistant: {first 200 chars of last assistant text}
pub struct RuleSummarizer;

impl Summarizer for RuleSummarizer {
    fn summarize(&self, messages: &[Message], action: PressureAction, _max_tokens: u32) -> String {
        let n = messages.len();
        let tokens: u32 = messages.iter().map(|m| m.token_count.unwrap_or(0)).sum();
        let tool_names: Vec<&str> = messages.iter().flat_map(|m| {
            match &m.content {
                Content::Parts(ps) => ps.iter().filter_map(|p| {
                    if let ContentPart::ToolCall { name, .. } = p { Some(name.as_str()) } else { None }
                }).collect::<Vec<_>>(),
                _ => vec![],
            }
        }).collect();
        let last_assistant = messages.iter().rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| m.content.as_text())
            .map(|t| if t.len() > 200 { &t[..200] } else { t })
            .unwrap_or("");

        let action_str = match action {
            PressureAction::SnipCompact     => "snip_compact",
            PressureAction::MicroCompact    => "micro_compact",
            PressureAction::ContextCollapse => "context_collapse",
            PressureAction::AutoCompact     => "auto_compact",
            PressureAction::None            => "none",
        };

        let mut s = format!(
            "[Compressed: {action_str}]\n{n} messages / {tokens} tokens archived\n"
        );
        if !tool_names.is_empty() {
            let unique: Vec<&str> = {
                let mut v = tool_names.clone();
                v.dedup();
                v
            };
            s.push_str(&format!("tools used: {}\n", unique.join(", ")));
        }
        if !last_assistant.is_empty() {
            s.push_str(&format!("last assistant output: {last_assistant}"));
        }
        s
    }
}
```

### C3. Compressor trait extended

Update `Compressor` to return a `CompressResult`:

```rust
pub struct CompressResult {
    pub tokens_saved: u32,
    pub summary:      Option<String>,         // produced by RuleSummarizer
    pub archived:     Vec<Message>,           // messages removed from partition
}

pub trait Compressor: Send + Sync {
    fn compress(
        &self,
        partition: &mut Partition,
        target_tokens: u32,
        summarizer: &dyn Summarizer,
        engine: &ContextTokenEngine,
    ) -> CompressResult;
}
```

`SnipCompactor` and `MicroCompactor` return `archived = vec![]` (content mutated in place, no messages removed). `CollapseCompactor` and `AutoCompactor` populate `archived` with the removed messages.

### C4. Archive store

```rust
/// Pluggable blob store for compressed history segments.
pub trait ArchiveStore: Send + Sync {
    /// Write bytes; return an opaque ref string (path, hash, or URL).
    fn write(&self, session_id: &str, seq: u64, data: &[u8]) -> Result<String, ArchiveError>;
}

/// Null implementation — discards archive data. Default when no path configured.
pub struct NullArchiveStore;

/// File-based store: `{root}/{session_id}/{seq}.jsonl`
pub struct FileArchiveStore { pub root: PathBuf }
```

`RuntimeOptions` gains `compression_store: Option<Arc<dyn ArchiveStore>>`. When `None`, the `NullArchiveStore` is used and `archive_ref` is omitted from `SessionEvent::Compressed`.

### C5. Runner integration

When the runner writes a `Compressed` event (currently `rust/src/runtime/runner.rs:592`), it:

1. Retrieves `CompressResult.archived` from the compressor.
2. Serialises `archived` to JSONL bytes.
3. Calls `archive_store.write(session_id, seq, bytes)` → `archive_ref`.
4. Appends `SessionEvent::Compressed` with `summary`, `summary_tokens`, `archive_ref`, `action`.

### C6. Wake / recovery injection

`RuntimeRunner::wake()` already reads `SessionEvent::Compressed` events (see `runner.rs:658`). Extend this to:

```
for each Compressed event in log:
    if event.summary is Some:
        inject a system message "[Compressed context: {event.turn}]\n{summary}"
        into the preload context before the next live message
```

This gives the agent a textual record of what was compressed without requiring the full archive.

### C7. Acceptance criteria — ✅ MET

- Old logs with `{ kind: "compressed", turn: N, archived_seq_range: [A, B] }` deserialise without error (all new fields `None` / empty). ✅
- After `AutoCompact` with `RuleSummarizer`, `SessionEvent::Compressed.summary` contains the tool names and last assistant excerpt. ✅
- `wake()` on a session with a `summary`-bearing `Compressed` event injects the summary text before the first live history turn. ✅ (t11 integration test)
- With `NullArchiveStore`, `archive_ref` is `None` and no files are written. ✅
- With `FileArchiveStore`, `{root}/{session_id}/{seq}.jsonl` exists after compression and contains the removed messages. ✅

**Phase C additions beyond original spec:**

- `ArchiveStore` gains a `read(archive_ref) -> Vec<Message>` method alongside `write()`.
- Rust/Node/Python runners use `reconstruct_messages_with_fallback()` on wake: loads archived messages via `read()` when `archive_ref` is set, falls back to summary injection on `MissingArchive`.
- New kernel types in `context/snapshot.rs`: `ContextPage`, `ContextSnapshot`, `ContextArchiveRef`, `ContextGcPolicy`, `ContextFault` (`PromptTooLong` / `MissingArchive` / `InvalidReplay`).
- `ContextFault` is the error type for `load_archive` callbacks across all SDK runners.

---

## 6. Phase D — Smart Compression Pipeline

_Phase D replaces destructive compactors with summarise-first variants. Implementation deferred until Phase A–C are shipped._

### D1. `Summarizer` trait (already defined in C2)

SDK injects `LLMSummarizer` (async LLM call) alongside `RuleSummarizer`. The pipeline always runs `RuleSummarizer` first (synchronous, no latency), then optionally upgrades the summary asynchronously via `LLMSummarizer`.

### D2. `SnipCompactor` — token-aware truncation

Replace byte-count `max_chars` with `max_tokens: u32`. Use `engine.truncate_message()` which preserves head and tail:

```
keep first (max_tokens / 2) tokens + keep last (max_tokens / 2) tokens
+ "[… N tokens omitted …]" in the middle
```

### D3. `MicroCompactor` — structured excerpt

Instead of `"[tool result cached: {call_id}]"`, produce:

```
[tool result: {call_id} | {name} | {token_count} tokens]
{first 30 tokens of output}
… [{remaining} tokens omitted] …
{last 10 tokens of output}
```

For structured JSON tool results, extract numeric fields and table headers before truncating.

### D4. `CollapseCompactor` — rolling summary

Instead of dropping messages, merge the oldest N turns into one `assistant`-role summary message via `RuleSummarizer`. N is chosen so the resulting summary fits in `target_tokens * 0.10`.

### D5. `AutoCompactor` — archive + LLM summary

The current 10-token placeholder becomes:

1. Dump entire history to archive.
2. Run `RuleSummarizer` → inject into `task_state.scratchpad` + working.
3. Optionally (SDK opt-in): schedule async `LLMSummarizer` call; result written back to `SessionLog` as a follow-up `Compressed` event with updated `summary`.

### D6. Tool result tiering

| Result size | Action |
|-------------|--------|
| < 200 tokens | Keep in full |
| 200–2 000 tokens | Keep with `SnipCompactor` excerpt |
| > 2 000 tokens | Archive full content; keep structured excerpt (D3) |

### D7. Preservation whitelist

Never compress:
- Messages from the most recent K turns (default K = 2, configurable).
- `ContentPart::ToolResult` entries whose `call_id` appears in `task_state.preserved_refs`.
- Messages with `role = System`.

---

## 7. Phase E — SDK & Application Alignment

### E1. SDK surface changes (all SDKs)

```typescript
// RuntimeOptions additions (Node / WASM)
interface RuntimeOptions {
  tokenizer?:           "char_approx" | "tiktoken_o200k" | "tiktoken_cl100k"
  compression_store?:   ArchiveStore   // Node: FileArchiveStore | null
  enable_plan_tool?:    boolean        // default: false
  summarizer?:          "rule" | "llm" // default: "rule"
}
```

```python
# Python additions
@dataclass
class RuntimeOptions:
    tokenizer: str = "char_approx"
    compression_store: Optional[ArchiveStore] = None
    enable_plan_tool: bool = False
    summarizer: str = "rule"
```

### E2. `repairEventsForRecovery` integration

The Node `session-repair.ts` currently recognises `run_started`, `llm_completed`, `tool_completed`. Extend:

```typescript
// When a Compressed event with summary is encountered during recovery:
if (event.kind === "compressed" && event.summary) {
  injectSummaryMessage(context, event.summary, event.turn)
}
```

### E3. QuantStrike application

- `daily_scan` writes `plan` to `ContextManager.init_task()` at run start: each ticker is a `PlanStep`.
- AKShare table results (>2 000 tokens) go through tool result tiering (D6): full table archived, excerpt (header + 5 rows + summary stats) kept in context.
- `task_state.progress` updated after each ticker analysis: `"Processed AAPL, TSLA (2/20)"`.

### E4. Documentation

- `docs/spec-runtime-v1.md`: update `compressed` event schema table with v1.1 fields.
- `docs/architecture.md`: replace "LLM summary (planned)" with Phase D description.
- Add `docs/spec-context-compression-v2.md` (this document) to `docs/index.md`.

---

## 8. Schema & API Compatibility

| Change | Compatibility | Notes |
|--------|--------------|-------|
| `SessionEvent::Compressed` adds 4 optional fields | Additive — old readers unaffected | serde `default` on all new fields |
| `LoopObservation::Compressed` adds `summary: Option<String>` | Additive | FFI bindings add field with default |
| `ContextPartitions` adds `task_state: TaskState` | Internal — not in any public SDK type | Default is empty TaskState |
| `Dashboard` fields deprecated | Deprecated, not removed | Kept until Phase E cleanup |
| `Compressor::compress()` signature changes | Breaking — internal trait | No external implementors in v0.x |
| `RuntimeOptions` new optional fields | Additive | All have defaults |

**Recommended version:** `0.2.0` — the `Compressor` trait signature change is internal but the token engine and working-partition semantics are a meaningful behaviour change.

---

## 9. Test Plan

### Unit tests

| Test | File | Assertion |
|------|------|-----------|
| `char_approx_count_cjk` | `token_engine_tests.rs` | `count("你好世界")` returns value > 0, no panic |
| `truncate_at_char_boundary` | `token_engine_tests.rs` | `truncate("你好世界", 1)` is valid UTF-8 |
| `count_truncated_le_max` | `token_engine_tests.rs` | `count(truncate(text, n)) <= n` for 50 random inputs |
| `task_state_compact_empty` | `task_state_tests.rs` | `TaskState::default().format_compact() == ""` |
| `task_state_compact_nonempty` | `task_state_tests.rs` | Rendered string contains goal, plan steps |
| `working_not_touched_by_pipeline` | `compression_tests.rs` | After `AutoCompact`, `task_state.goal` unchanged |
| `rule_summarizer_includes_tool_names` | `summarizer_tests.rs` | Summary contains names of tools used |
| `compressed_event_backward_compat` | `session_tests.rs` | Old `{kind:"compressed", turn:1, archived_seq_range:[0,0]}` deserialises to `summary: None` |

### Integration tests

| Scenario | Expected |
|----------|----------|
| Long session → Snip→Micro→Collapse → agent queried on plan step 3 | Agent answers from `task_state` in `system_text` |
| `AutoCompact` with `FileArchiveStore` | Archive file written; `SessionEvent.archive_ref` points to it |
| `wake()` on session with summary-bearing Compressed event | Summary text present in first rendered turn |
| CJK tool output > 2 000 tokens compressed via MicroCompactor | Result excerpt is valid UTF-8; key numeric fields retained |

### Regression tests

| Scenario | Expected |
|----------|----------|
| Existing Node `wake-recovery.test.ts` | Still passes after runner changes |
| `session-repair.test.ts` | Handles `Compressed` events with missing `summary` gracefully |
| QuantStrike `daily_scan` with AKShare large table | After compression, watchlist symbols still addressable |

---

## 10. Open Decisions

| # | Question | Default / Recommendation |
|---|----------|--------------------------|
| 1 | LLM summariser — synchronous in compress path? | **No.** Rule summary is synchronous; LLM upgrade is async, written back as a second `Compressed` event. |
| 2 | Archive storage — inline in SessionLog vs external file? | **External file** (`FileArchiveStore`) for payloads > 4 KB; `SessionLog` holds `archive_ref` + `summary` only. |
| 3 | `TaskState` render location — `system_text` vs first `user` message? | **`system_text`** (after dashboard) — avoids consecutive user messages and is safe across all providers. |
| 4 | Tokeniser default in `RuntimeOptions` | **`"char_approx"`** to preserve current behaviour; users opt in to real tokeniser. |
| 5 | `enable_plan_tool` default | **`false`** — plan tool changes which tool calls reach `ExecutionPlane`; opt-in change only. |

---

## 11. Implementation Order

```
Phase A (token engine)    ✅ SHIPPED
  A1 → A2 → A3 → A4 (config)

Phase B (working task state) ✅ SHIPPED
  B1 → B2 → B3 → B4 (write timing) → B5 (renewal) → B6 (plan tool)
  + Artifacts 6th partition + KernelInputEvent::PushArtifact
  + ContextSnapshot / ContextPage / ContextArchiveRef types

Phase C (archive) ✅ SHIPPED
  C1 (schema) → C2 (summariser) → C3 (compressor trait) → C4 (store) → C5 (runner) → C6 (wake)
  + ArchiveStore.read() + reconstruct_messages_with_fallback() + ContextFault

Phase D (smart pipeline) ⏸ DEFERRED — depends on C stabilization
  D1 → D2/D3 (compactors) → D4/D5 (CollapseCompactor, AutoCompactor) → D6/D7 (tiering)

Phase E (SDK + docs) ⏸ DEFERRED
  E1 → E2 → E3 → E4
```

**MVP (3 weeks):** ✅ **SHIPPED** — Phase A + B + C with `RuleSummarizer`. Resolves "compression destroys everything" and "plan is lost after AutoCompact".

**Next:** Phase D (LLM summariser) or Phase 3 Capability Bus (already partially implemented).
