use serde::{Deserialize, Serialize};

/// One entry in the compression log — records what happened at each compression event.
/// All tiers write here; the log is append-only and never overwritten.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionEntry {
    /// Compression tier label: snip_compact | micro_compact | context_collapse | auto_compact
    pub action: String,
    /// Human-readable summary (tool names, message counts, token counts).
    /// Empty for Snip/Micro which only record truncation stats.
    pub summary: String,
}

/// Persistent task state that lives in the working partition.
/// Survives compression, renewal, and wake/resume cycles because the working
/// partition is `compressible = false`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskState {
    /// Primary objective for this run. Set at `run_started`, immutable thereafter.
    pub goal: String,
    /// Acceptance criteria copied from `RunStarted`.
    pub criteria: Vec<String>,
    /// Ordered plan steps.
    pub plan: Vec<PlanStep>,
    /// Index of the step currently executing (0-based). None before planning.
    pub current_step: Option<usize>,
    /// Free-text progress note updated after each significant action.
    pub progress: String,
    /// Ephemeral scratch space for model use. Cleared on renewal. NOT used by the
    /// compression pipeline (use compression_log instead).
    pub scratchpad: String,
    /// Reasons the current step cannot proceed.
    pub blocked_on: Vec<String>,
    /// Explicit durable user directives / standing constraints (e.g. "don't do X"). Unlike
    /// runtime signals, these are intentionally persisted across compression and renewal.
    /// Bounded + recency-ordered (oldest dropped past [`MAX_DIRECTIVES`]); newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
    /// Call IDs or artifact hashes that must be preserved from compression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserved_refs: Vec<String>,
    /// Rolling log of recent *task* activity — one entry per turn, each a compact summary of that
    /// turn's tool calls (e.g. "module_read, module_list"). Kernel-maintained from REAL tool
    /// activity (not model-curated), so the State turn always shows forward motion even when the
    /// model never maintains `plan`. Lives in the volatile State turn (out of the cacheable prefix),
    /// so updating it never churns the prompt cache. Bounded + recency-ordered; newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_actions: Vec<String>,
    /// Rolling log of compression events, newest last. Bounded at [`MAX_COMPRESSION_LOG`]
    /// (oldest dropped past the cap, counted in `compression_log_dropped`).
    /// Rendered into systemVolatile so the model always sees compression history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compression_log: Vec<CompressionEntry>,
    /// Entries dropped from `compression_log` past its cap — kept so the render stays honest
    /// about how much history is no longer visible.
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub compression_log_dropped: u64,
}

fn u64_is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub label: String,
    pub done: bool,
}

impl PlanStep {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            done: false,
        }
    }
}

/// Maximum durable directives retained; past this the oldest is dropped (recency window).
pub const MAX_DIRECTIVES: usize = 8;

/// Maximum recent action-turns retained for the recency footer (bounded ring).
pub const MAX_RECENT_ACTIONS: usize = 6;

/// Compression-log entries retained in state (rolling window; render shows the last 3).
pub const MAX_COMPRESSION_LOG: usize = 64;

/// Partial update applied by the SDK or via `update_plan` meta-tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub plan: Option<Vec<String>>,
    pub current_step: Option<usize>,
    pub progress: Option<String>,
    pub scratchpad: Option<String>,
    pub blocked_on: Option<Vec<String>>,
    pub preserved_refs: Option<Vec<String>>,
    /// Replace the durable directive list wholesale (SDK/model curation).
    pub directives: Option<Vec<String>>,
}

impl TaskState {
    /// Compact text block for embedding in `system_text`.
    /// Returns an empty string when the task has not been initialised.
    pub fn format_compact(&self) -> String {
        if self.goal.is_empty() && self.plan.is_empty() && self.progress.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push(format!("[TASK STATE] goal: {}", self.goal));

        if !self.criteria.is_empty() {
            lines.push(format!("criteria: {}", self.criteria.join(" | ")));
        }

        // Active directives render right after the goal — highest salience after the objective, so
        // a recent user command keeps its imperative force across compaction/renewal.
        if !self.directives.is_empty() {
            lines.push("active_directives (most recent last):".to_string());
            for d in &self.directives {
                lines.push(format!("  - {d}"));
            }
        }

        if !self.plan.is_empty() {
            lines.push("plan:".to_string());
            for (i, step) in self.plan.iter().enumerate() {
                let marker = if step.done {
                    "done"
                } else if Some(i) == self.current_step {
                    "active"
                } else {
                    "todo"
                };
                lines.push(format!("  [{}] {}. {}", marker, i + 1, step.label));
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

        // Render only the most recent compression events (fixed cap of 3). A wider budgeted
        // window was tried and withdrawn: live A/B at n≤4 (12-PR review @ 2048, DeepSeek)
        // could not distinguish it from provider drift — a control run on the unmodified
        // kernel showed the same failures — and a window full of old `tool X args` digest
        // lines is plausible re-execution bait. Absent replay-lab evidence FOR widening, the
        // proven shape stays. Older digests remain in `compression_log` (bounded at
        // [`MAX_COMPRESSION_LOG`]); making their *content* durably useful to the model is
        // semantic-summary (P2) territory, not a bigger raw window.
        if !self.compression_log.is_empty() {
            lines.push("compression_history:".to_string());
            let start = self.compression_log.len().saturating_sub(3);
            for entry in &self.compression_log[start..] {
                if entry.summary.is_empty() {
                    lines.push(format!("  [{}]", entry.action));
                } else {
                    lines.push(format!("  [{}] {}", entry.action, entry.summary));
                }
            }
        }

        lines.join("\n")
    }

    /// Record a durable user directive (deduped against the most recent, recency-capped at
    /// [`MAX_DIRECTIVES`]). Newest is appended last; the oldest is dropped past the cap so the
    /// channel stays bounded across a long session.
    pub fn record_directive(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        // Re-issuing the same directive moves it to most-recent rather than duplicating.
        self.directives.retain(|d| d != &text);
        self.directives.push(text);
        if self.directives.len() > MAX_DIRECTIVES {
            let overflow = self.directives.len() - MAX_DIRECTIVES;
            self.directives.drain(0..overflow);
        }
    }

    /// Record one turn's tool activity into the recency log (kernel-driven). `summary` is a compact
    /// string of the turn's task tool names; blank input is ignored. Bounded at
    /// [`MAX_RECENT_ACTIONS`] (oldest dropped past the cap).
    pub fn note_actions(&mut self, summary: impl Into<String>) {
        let summary = summary.into();
        if summary.trim().is_empty() {
            return;
        }
        self.recent_actions.push(summary);
        if self.recent_actions.len() > MAX_RECENT_ACTIONS {
            let overflow = self.recent_actions.len() - MAX_RECENT_ACTIONS;
            self.recent_actions.drain(0..overflow);
        }
    }

    /// Append a compression event to the log (bounded at [`MAX_COMPRESSION_LOG`]; the oldest
    /// entries are dropped past the cap and counted so the render can report them).
    pub fn log_compression(&mut self, action: &str, summary: String) {
        self.compression_log.push(CompressionEntry {
            action: action.to_string(),
            summary,
        });
        if self.compression_log.len() > MAX_COMPRESSION_LOG {
            let overflow = self.compression_log.len() - MAX_COMPRESSION_LOG;
            self.compression_log.drain(0..overflow);
            self.compression_log_dropped += overflow as u64;
        }
    }

    pub fn apply(&mut self, update: TaskUpdate) {
        if let Some(plan) = update.plan {
            self.plan = plan.into_iter().map(PlanStep::new).collect();
        }
        if let Some(step) = update.current_step {
            self.current_step = Some(step);
        }
        if let Some(p) = update.progress {
            self.progress = p;
        }
        if let Some(s) = update.scratchpad {
            self.scratchpad = s;
        }
        if let Some(b) = update.blocked_on {
            self.blocked_on = b;
        }
        if let Some(r) = update.preserved_refs {
            self.preserved_refs = r;
        }
        if let Some(d) = update.directives {
            self.directives = d;
            if self.directives.len() > MAX_DIRECTIVES {
                let overflow = self.directives.len() - MAX_DIRECTIVES;
                self.directives.drain(0..overflow);
            }
        }
    }

    /// Open steps (not yet done), for renewal handoff.
    pub fn open_steps(&self) -> Vec<String> {
        self.plan
            .iter()
            .filter(|s| !s.done)
            .map(|s| s.label.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_compact_is_empty_string() {
        assert_eq!(TaskState::default().format_compact(), "");
    }

    #[test]
    fn goal_only_renders() {
        let ts = TaskState {
            goal: "Build it".to_string(),
            ..Default::default()
        };
        let s = ts.format_compact();
        assert!(s.contains("[TASK STATE] goal: Build it"));
    }

    #[test]
    fn plan_markers_correct() {
        let ts = TaskState {
            goal: "g".to_string(),
            plan: vec![
                PlanStep {
                    label: "step1".to_string(),
                    done: true,
                },
                PlanStep {
                    label: "step2".to_string(),
                    done: false,
                },
                PlanStep {
                    label: "step3".to_string(),
                    done: false,
                },
            ],
            current_step: Some(1),
            ..Default::default()
        };
        let s = ts.format_compact();
        assert!(s.contains("[done] 1. step1"));
        assert!(s.contains("[active] 2. step2"));
        assert!(s.contains("[todo] 3. step3"));
    }

    #[test]
    fn open_steps_excludes_done() {
        let ts = TaskState {
            goal: "g".to_string(),
            plan: vec![
                PlanStep {
                    label: "a".to_string(),
                    done: true,
                },
                PlanStep {
                    label: "b".to_string(),
                    done: false,
                },
            ],
            ..Default::default()
        };
        assert_eq!(ts.open_steps(), vec!["b"]);
    }

    #[test]
    fn record_directive_dedups_caps_and_orders_by_recency() {
        let mut ts = TaskState::default();
        ts.record_directive("don't touch the db schema");
        ts.record_directive("use 2-space indent");
        // Re-issuing moves to most-recent, no duplicate.
        ts.record_directive("don't touch the db schema");
        assert_eq!(
            ts.directives,
            ["use 2-space indent", "don't touch the db schema"]
        );

        // Bounded at MAX_DIRECTIVES — oldest dropped.
        let mut ts = TaskState::default();
        for i in 0..(MAX_DIRECTIVES + 3) {
            ts.record_directive(format!("rule {i}"));
        }
        assert_eq!(ts.directives.len(), MAX_DIRECTIVES);
        assert_eq!(ts.directives.first().unwrap(), "rule 3"); // 0..2 dropped
        assert_eq!(
            ts.directives.last().unwrap(),
            &format!("rule {}", MAX_DIRECTIVES + 2)
        );

        // Blank is ignored.
        let mut ts = TaskState::default();
        ts.record_directive("  ");
        assert!(ts.directives.is_empty());
    }

    #[test]
    fn directives_render_after_goal() {
        let mut ts = TaskState {
            goal: "ship it".to_string(),
            ..Default::default()
        };
        ts.record_directive("don't break the public API");
        let s = ts.format_compact();
        assert!(s.contains("active_directives"));
        assert!(s.contains("- don't break the public API"));
        // Renders after the goal line.
        assert!(s.find("goal: ship it").unwrap() < s.find("don't break the public API").unwrap());
    }

    #[test]
    fn apply_replaces_directives_and_caps() {
        let mut ts = TaskState::default();
        ts.apply(TaskUpdate {
            directives: Some((0..(MAX_DIRECTIVES + 2)).map(|i| format!("d{i}")).collect()),
            ..Default::default()
        });
        assert_eq!(ts.directives.len(), MAX_DIRECTIVES);
    }

    #[test]
    fn compression_render_shows_exactly_the_last_three_digests() {
        // Pin the 3-entry window (see format_compact for why widening was withdrawn) so a
        // future widening must re-justify itself with replay-lab evidence.
        let mut ts = TaskState::default();
        ts.goal = "review PRs".into();
        for n in 1..=10 {
            ts.log_compression("auto_compact", format!("digest {n}"));
        }
        let rendered = ts.format_compact();
        assert!(!rendered.contains("digest 7\n"));
        assert!(rendered.contains("digest 8"));
        assert!(rendered.contains("digest 9"));
        assert!(rendered.contains("digest 10"));
    }

    #[test]
    fn compression_log_is_bounded_and_counts_drops() {
        let mut ts = TaskState::default();
        for n in 0..(MAX_COMPRESSION_LOG + 5) {
            ts.log_compression("micro_compact", format!("d{n}"));
        }
        assert_eq!(ts.compression_log.len(), MAX_COMPRESSION_LOG);
        assert_eq!(ts.compression_log_dropped, 5);
        assert_eq!(ts.compression_log[0].summary, "d5");
    }

    #[test]
    fn apply_updates_fields() {
        let mut ts = TaskState::default();
        ts.apply(TaskUpdate {
            progress: Some("half done".to_string()),
            blocked_on: Some(vec!["waiting for data".to_string()]),
            ..Default::default()
        });
        assert_eq!(ts.progress, "half done");
        assert_eq!(ts.blocked_on, ["waiting for data"]);
    }
}
