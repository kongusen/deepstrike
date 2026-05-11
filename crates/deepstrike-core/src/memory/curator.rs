use std::collections::HashSet;

use crate::memory::semantic::MemoryEntry;
use crate::memory::trace_analyzer::{InsightKind, TraceInsight};

/// When a new insight is similar to an existing entry, which one wins?
#[derive(Debug, Clone)]
pub enum ConflictResolution {
    /// Always replace the existing entry with the newer insight.
    PreferNewer,
    /// Keep whichever entry has the higher confidence score.
    PreferHigherConfidence,
}

#[derive(Debug, Clone)]
pub struct CurationPolicy {
    /// Jaccard similarity threshold above which two entries are considered duplicates. Default: 0.65.
    pub similarity_threshold: f64,
    /// Maximum total entries in the long-term store after this run. Default: 500.
    pub max_entries: usize,
    pub conflict_resolution: ConflictResolution,
    /// Insights below this confidence are silently dropped. Default: 0.3.
    pub min_confidence: f64,
}

impl Default for CurationPolicy {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.65,
            max_entries: 500,
            conflict_resolution: ConflictResolution::PreferNewer,
            min_confidence: 0.3,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CurationStats {
    pub insights_processed: usize,
    pub duplicates_removed: usize,
    pub conflicts_resolved: usize,
    pub entries_added: usize,
}

/// The pure-computation delta the SDK applies to the long-term memory store.
#[derive(Debug, Clone)]
pub struct CurationResult {
    pub to_add: Vec<MemoryEntry>,
    /// Indices into the `existing` slice passed to `curate` — SDK removes these entries.
    pub to_remove_indices: Vec<usize>,
    pub stats: CurationStats,
}

pub struct MemoryCurator {
    pub policy: CurationPolicy,
}

impl MemoryCurator {
    pub fn new(policy: CurationPolicy) -> Self {
        Self { policy }
    }

    /// Produce a delta (add / remove) from `insights` relative to `existing` entries.
    ///
    /// `now_ms` is injected by the SDK — the kernel never reads system time.
    pub fn curate(
        &self,
        insights: &[TraceInsight],
        existing: &[MemoryEntry],
        now_ms: u64,
    ) -> CurationResult {
        let mut stats = CurationStats { insights_processed: insights.len(), ..Default::default() };
        let mut to_add: Vec<MemoryEntry> = Vec::new();
        let mut to_remove_indices: Vec<usize> = Vec::new();

        for insight in insights {
            if insight.confidence < self.policy.min_confidence {
                continue;
            }

            let candidate = insight_to_entry(insight, now_ms);

            // Check against existing entries.
            let mut conflict_idx: Option<usize> = None;
            for (idx, existing_entry) in existing.iter().enumerate() {
                if to_remove_indices.contains(&idx) {
                    continue; // already evicted this run
                }
                if jaccard(&candidate.text, &existing_entry.text) >= self.policy.similarity_threshold
                {
                    conflict_idx = Some(idx);
                    break;
                }
            }

            if let Some(idx) = conflict_idx {
                let existing_entry = &existing[idx];
                let keep_new = match self.policy.conflict_resolution {
                    ConflictResolution::PreferNewer => true,
                    ConflictResolution::PreferHigherConfidence => {
                        candidate.score >= existing_entry.score
                    }
                };
                if keep_new {
                    to_remove_indices.push(idx);
                    stats.conflicts_resolved += 1;
                } else {
                    stats.duplicates_removed += 1;
                    continue;
                }
            }

            // Deduplicate within this batch.
            let dup_in_batch = to_add
                .iter()
                .any(|e| jaccard(&candidate.text, &e.text) >= self.policy.similarity_threshold);
            if dup_in_batch {
                stats.duplicates_removed += 1;
                continue;
            }

            to_add.push(candidate);
            stats.entries_added += 1;
        }

        to_remove_indices.sort_unstable();
        to_remove_indices.dedup();

        // Trim to_add if the store would exceed max_entries.
        let surviving_existing = existing.len().saturating_sub(to_remove_indices.len());
        let headroom = self.policy.max_entries.saturating_sub(surviving_existing);
        to_add.truncate(headroom);
        stats.entries_added = to_add.len();

        CurationResult { to_add, to_remove_indices, stats }
    }
}

// --- helpers -----------------------------------------------------------------

fn insight_to_entry(insight: &TraceInsight, now_ms: u64) -> MemoryEntry {
    let text = match &insight.kind {
        InsightKind::RepeatedToolError { tool_name, error_count, sample_error } => {
            format!("Tool '{}' failed {} times; pattern: {}", tool_name, error_count, sample_error)
        }
        InsightKind::SuccessfulToolSequence { tools, context_hint } => {
            format!("Successful sequence [{}] for: {}", tools.join(" → "), context_hint)
        }
        InsightKind::LongReasoning { summary_hint } => summary_hint.clone(),
        InsightKind::Synthesized { text } => text.clone(),
    };
    let metadata = serde_json::json!({
        "kind": insight.kind.tag(),
        "confidence": insight.confidence,
        "session_id": insight.session_id,
        "extracted_at_ms": now_ms,
    });
    MemoryEntry { text, score: insight.confidence, metadata }
}

fn jaccard(a: &str, b: &str) -> f64 {
    let sa: HashSet<&str> = a.split_whitespace().collect();
    let sb: HashSet<&str> = b.split_whitespace().collect();
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 0.0 } else { inter as f64 / union as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::trace_analyzer::{AnalysisPolicy, InsightKind, TraceAnalyzer, TraceInsight};
    use pretty_assertions::assert_eq;

    fn curator() -> MemoryCurator {
        MemoryCurator::new(CurationPolicy::default())
    }

    fn error_insight(tool: &str, confidence: f64) -> TraceInsight {
        TraceInsight {
            kind: InsightKind::RepeatedToolError {
                tool_name: tool.to_string(),
                error_count: 3,
                sample_error: "permission denied".to_string(),
            },
            confidence,
            session_id: "s1".to_string(),
        }
    }

    fn existing_entry(text: &str, score: f64) -> MemoryEntry {
        MemoryEntry { text: text.to_string(), score, metadata: serde_json::Value::Null }
    }

    #[test]
    fn adds_new_insights_when_no_existing() {
        let result = curator().curate(&[error_insight("bash", 0.8)], &[], 0);
        assert_eq!(result.to_add.len(), 1);
        assert!(result.to_remove_indices.is_empty());
        assert_eq!(result.stats.entries_added, 1);
    }

    #[test]
    fn skips_low_confidence_insights() {
        // min_confidence is 0.3; pass 0.1
        let result = curator().curate(&[error_insight("bash", 0.1)], &[], 0);
        assert!(result.to_add.is_empty());
        assert_eq!(result.stats.entries_added, 0);
    }

    #[test]
    fn prefer_newer_replaces_similar_existing() {
        let existing = vec![existing_entry(
            "Tool 'bash' failed 2 times; pattern: permission denied",
            0.4,
        )];
        let result = curator().curate(&[error_insight("bash", 0.8)], &existing, 1000);
        assert_eq!(result.to_add.len(), 1);
        assert_eq!(result.to_remove_indices, vec![0]);
        assert_eq!(result.stats.conflicts_resolved, 1);
    }

    #[test]
    fn prefer_higher_confidence_keeps_existing_when_better() {
        let policy = CurationPolicy {
            conflict_resolution: ConflictResolution::PreferHigherConfidence,
            ..Default::default()
        };
        let curator = MemoryCurator::new(policy);
        let existing =
            vec![existing_entry("Tool 'bash' failed 3 times; pattern: permission denied", 0.95)];
        // New insight has lower confidence → existing wins.
        let result = curator.curate(&[error_insight("bash", 0.5)], &existing, 0);
        assert!(result.to_add.is_empty());
        assert!(result.to_remove_indices.is_empty());
        assert_eq!(result.stats.duplicates_removed, 1);
    }

    #[test]
    fn deduplicates_within_batch() {
        // Two insights that produce nearly identical text.
        let insights = vec![error_insight("bash", 0.8), error_insight("bash", 0.7)];
        let result = curator().curate(&insights, &[], 0);
        assert_eq!(result.to_add.len(), 1);
        assert_eq!(result.stats.duplicates_removed, 1);
    }

    #[test]
    fn respects_max_entries_headroom() {
        let policy = CurationPolicy { max_entries: 2, ..Default::default() };
        let curator = MemoryCurator::new(policy);
        let existing = vec![
            existing_entry("unrelated entry one", 0.5),
            existing_entry("unrelated entry two", 0.5),
        ];
        // Store is already full (2 existing, max=2) → nothing fits.
        let insights = vec![error_insight("bash", 0.8)];
        let result = curator.curate(&insights, &existing, 0);
        assert!(result.to_add.is_empty());
    }

    #[test]
    fn end_to_end_with_trace_analyzer() {
        use crate::types::message::{ContentPart, ToolCall};
        use compact_str::CompactString;

        let mut call_msg = crate::types::message::Message::assistant("");
        call_msg.tool_calls = vec![
            ToolCall {
                id: CompactString::new("c1"),
                name: CompactString::new("bash"),
                arguments: serde_json::Value::Null,
            },
            ToolCall {
                id: CompactString::new("c2"),
                name: CompactString::new("bash"),
                arguments: serde_json::Value::Null,
            },
        ];
        let err_msg1 = crate::types::message::Message::tool(vec![ContentPart::ToolResult {
            call_id: CompactString::new("c1"),
            output: "permission denied".to_string(),
            is_error: true,
        }]);
        let err_msg2 = crate::types::message::Message::tool(vec![ContentPart::ToolResult {
            call_id: CompactString::new("c2"),
            output: "permission denied".to_string(),
            is_error: true,
        }]);

        let messages = vec![call_msg, err_msg1, err_msg2];
        let analyzer = TraceAnalyzer::new(AnalysisPolicy::default());
        let insights = analyzer.analyze("s1", &messages);
        assert!(!insights.is_empty());

        let result = curator().curate(&insights, &[], 42_000);
        assert!(!result.to_add.is_empty());
        assert!(
            result.to_add[0].metadata["kind"] == "repeated_tool_error"
                || result.to_add[0].metadata["kind"] == "synthesized"
        );
        assert_eq!(result.to_add[0].metadata["extracted_at_ms"], 42_000);
    }
}
