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
    /// Explicit durable user directives / standing constraints (e.g. "don't do X"). Unlike
    /// runtime signals, these are intentionally persisted across compression and renewal.
    /// Bounded + recency-ordered (oldest dropped past [`MAX_DIRECTIVES`]); newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directives: Vec<String>,
    /// Call IDs or artifact hashes that must be preserved from compression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserved_refs: Vec<String>,
    /// Rolling log of compression events, newest last. Bounded at [`MAX_COMPRESSION_LOG`]
    /// (oldest dropped past the cap, counted in `compression_log_dropped`).
    /// Rendered into systemVolatile so the model always sees compression history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compression_log: Vec<CompressionEntry>,
    /// Entries dropped from `compression_log` past its cap — kept so the render stays honest
    /// about how much history is no longer visible.
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub compression_log_dropped: u64,
    /// Plan task 9.1 · notes the model wrote with the `note` tool. Part of the working partition,
    /// so compaction never touches them; rendered in the state turn. Oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Notes written or rewritten so far (a rewrite of one title counts again).
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub notes_written: u32,
    /// Notes the model retired.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub notes_retired: u32,
    /// The last item id when a note was last written: a note written without explicit evidence
    /// derives from the tool results that arrived after it.
    #[serde(default, skip_serializing_if = "u64_is_zero")]
    pub notes_watermark: u64,
}

fn u32_is_zero(value: &u32) -> bool {
    *value == 0
}

/// Plan task 9.1 · what a note holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// Working state of this session the model derived and could not read again: running counts,
    /// lists of what is done, intermediate conclusions.
    #[default]
    State,
    /// A rule or lesson learned: how something works, what to avoid.
    Rule,
    /// An open question, retired once answered.
    Question,
    /// An index: where things are, which notes cover what.
    Index,
}

impl NoteKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Rule => "rule",
            Self::Question => "question",
            Self::Index => "index",
        }
    }
}

/// Plan task 9.1 · a note the model wrote. Its title is its identity: writing the same title
/// again rewrites it in place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub title: String,
    pub kind: NoteKind,
    pub body: String,
    /// Where it came from, as the model cited it (call ids, paths, arguments).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    /// Titles of related notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// The note's id in the item graph (its `derived_from` / `links_to` edges start here).
    pub item: u64,
    /// The turn it was last written.
    pub turn: u32,
}

impl Note {
    /// The note's line in the state turn.
    pub fn render(&self) -> String {
        super::voice::note_line(self)
    }
}

pub use super::voice::NOTES_HEADING;

impl TaskState {
    /// The notes block as rendered in the state turn, or empty without notes.
    pub fn render_notes(&self) -> String {
        super::voice::notes_block(&self.notes)
    }

    /// The index of the note titled `title` (case and surrounding space ignored).
    pub fn note_index(&self, title: &str) -> Option<usize> {
        let wanted = title.trim().to_lowercase();
        self.notes.iter().position(|note| note.title.trim().to_lowercase() == wanted)
    }
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

/// Compression-log entries retained in state (rolling window; render shows the last 3).
pub const MAX_COMPRESSION_LOG: usize = 64;

/// Partial update applied by the SDK or via `update_plan` meta-tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub plan: Option<Vec<String>>,
    pub current_step: Option<usize>,
    pub preserved_refs: Option<Vec<String>>,
    /// Replace the durable directive list wholesale (SDK/model curation).
    pub directives: Option<Vec<String>>,
}

impl TaskState {
    /// Compact text block for embedding in `system_text`.
    /// Returns an empty string when the task has not been initialised.
    pub fn format_compact(&self) -> String {
        super::voice::task_state_block(self)
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
            plan: Some(vec!["read".to_string(), "write".to_string()]),
            current_step: Some(1),
            ..Default::default()
        });
        assert_eq!(ts.plan.len(), 2);
        assert_eq!(ts.current_step, Some(1));
    }
}
