/// Idle-time memory consolidation pipeline — the agent's "dreaming" cycle.
///
/// # Two-phase flow
///
/// ```text
/// Phase 1 — Rule-based analysis (synchronous, in-kernel)
/// ┌─────────────────────────────────────────────────┐
/// │ IdleEvent::Trigger { sessions, memories, now }  │
/// │   → TraceAnalyzer   (repeated errors, seqs…)   │
/// │   → SynthesisPromptBuilder (assembles prompt)   │
/// │   → IdleAction::SynthesizeInsights { messages } │ ← SDK calls LLM
/// └─────────────────────────────────────────────────┘
///
/// Phase 2 — LLM synthesis + curation (after SDK returns)
/// ┌─────────────────────────────────────────────────┐
/// │ IdleEvent::SynthesisResult { content }          │
/// │   → SynthesisResponseParser (JSON → insights)   │
/// │   → merge seed + synthesized insights           │
/// │   → MemoryCurator (dedup / conflict / trim)     │
/// │   → IdleAction::CommitMemories { delta }        │ ← SDK writes store
/// └─────────────────────────────────────────────────┘
/// ```
use crate::memory::curator::{CurationPolicy, CurationResult, CurationStats, MemoryCurator};
use crate::memory::durable::SessionData;
use crate::memory::semantic::MemoryEntry;
use crate::memory::synthesis::{SynthesisPolicy, SynthesisPromptBuilder, SynthesisResponseParser};
use crate::memory::trace_analyzer::{AnalysisPolicy, TraceAnalyzer, TraceInsight};
use crate::types::message::Message;

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IdleResult {
    pub sessions_processed: usize,
    /// Total insights (rule-based + synthesized) before curation.
    pub insights_extracted: usize,
    pub stats: CurationStats,
}

// ---------------------------------------------------------------------------
// State machine types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum IdlePhase {
    Idle,
    /// Rule-based analysis complete; waiting for the SDK to return LLM output.
    SynthesisPending {
        seed_insights: Vec<TraceInsight>,
        existing_memories: Vec<MemoryEntry>,
        now_ms: u64,
        sessions_processed: usize,
    },
    Done { result: IdleResult },
}

pub enum IdleEvent {
    /// SDK provides raw sessions + current memory snapshot; kernel does the rest.
    Trigger {
        sessions: Vec<SessionData>,
        existing_memories: Vec<MemoryEntry>,
        /// Wall-clock ms injected by the SDK — kernel never reads system time.
        now_ms: u64,
    },
    /// SDK feeds back the LLM's text response from the synthesis call.
    SynthesisResult { content: String },
    Abort,
}

pub enum IdleAction {
    /// Call the LLM with `messages`, then feed `IdleEvent::SynthesisResult`.
    SynthesizeInsights { messages: Vec<Message> },
    /// Apply `result` delta to the SemanticMemory store, then call `reset()`.
    CommitMemories { agent_id: String, result: CurationResult, run_result: IdleResult },
    /// No sessions to process this cycle.
    Noop,
    Aborted,
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IdlePolicy {
    pub agent_id: String,
    /// Sessions processed per idle cycle. Default: 20.
    pub max_sessions_per_run: usize,
    pub analysis: AnalysisPolicy,
    pub curation: CurationPolicy,
    pub synthesis: SynthesisPolicy,
}

impl IdlePolicy {
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            max_sessions_per_run: 20,
            analysis: AnalysisPolicy::default(),
            curation: CurationPolicy::default(),
            synthesis: SynthesisPolicy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Pure state machine — no I/O, no async.
pub struct IdlePipeline {
    pub phase: IdlePhase,
    policy: IdlePolicy,
    analyzer: TraceAnalyzer,
    curator: MemoryCurator,
    prompt_builder: SynthesisPromptBuilder,
}

impl IdlePipeline {
    pub fn new(policy: IdlePolicy) -> Self {
        let analyzer = TraceAnalyzer::new(policy.analysis.clone());
        let curator = MemoryCurator::new(policy.curation.clone());
        let prompt_builder = SynthesisPromptBuilder::new(policy.synthesis.clone());
        Self { phase: IdlePhase::Idle, policy, analyzer, curator, prompt_builder }
    }

    pub fn is_idle(&self) -> bool {
        matches!(self.phase, IdlePhase::Idle)
    }

    pub fn feed(&mut self, event: IdleEvent) -> IdleAction {
        match event {
            // -- Abort -------------------------------------------------------
            IdleEvent::Abort => {
                self.phase = IdlePhase::Idle;
                IdleAction::Aborted
            }

            // -- Phase 1: rule-based analysis + prompt assembly ---------------
            IdleEvent::Trigger { sessions, existing_memories, now_ms } => {
                if sessions.is_empty() {
                    return IdleAction::Noop;
                }

                let session_tuples: Vec<(String, Vec<Message>)> = sessions
                    .into_iter()
                    .take(self.policy.max_sessions_per_run)
                    .map(|s| (s.session_id, s.messages))
                    .collect();
                let sessions_processed = session_tuples.len();

                // Rule-based seed insights (pure computation).
                let seed_insights = self.analyzer.analyze_batch(&session_tuples);

                // Build LLM prompt (pure computation).
                let messages = self.prompt_builder.build(&session_tuples, &seed_insights);

                self.phase = IdlePhase::SynthesisPending {
                    seed_insights,
                    existing_memories,
                    now_ms,
                    sessions_processed,
                };

                IdleAction::SynthesizeInsights { messages }
            }

            // -- Phase 2: parse LLM output + curate --------------------------
            IdleEvent::SynthesisResult { content } => {
                // Extract pending state; reset to Idle on unexpected phase.
                let (seed_insights, existing_memories, now_ms, sessions_processed) =
                    match std::mem::replace(&mut self.phase, IdlePhase::Idle) {
                        IdlePhase::SynthesisPending {
                            seed_insights,
                            existing_memories,
                            now_ms,
                            sessions_processed,
                        } => (seed_insights, existing_memories, now_ms, sessions_processed),
                        other => {
                            self.phase = other;
                            return IdleAction::Aborted;
                        }
                    };

                // Parse LLM response (pure computation).
                let synthesized = SynthesisResponseParser::parse("synthetic", &content);

                // Merge: rule-based seeds first, then LLM-synthesized.
                let mut all_insights = seed_insights;
                all_insights.extend(synthesized);
                let insights_extracted = all_insights.len();

                // Curate the combined set against the existing memory store.
                let curation_result =
                    self.curator.curate(&all_insights, &existing_memories, now_ms);
                let stats = curation_result.stats.clone();

                let run_result = IdleResult { sessions_processed, insights_extracted, stats };
                self.phase = IdlePhase::Done { result: run_result.clone() };

                IdleAction::CommitMemories {
                    agent_id: self.policy.agent_id.clone(),
                    result: curation_result,
                    run_result,
                }
            }
        }
    }

    /// Reset to `Idle` after handling `CommitMemories`, allowing the next cycle.
    pub fn reset(&mut self) {
        self.phase = IdlePhase::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::durable::SessionData;
    use crate::types::message::{ContentPart, Message, ToolCall};
    use compact_str::CompactString;

    fn pipeline() -> IdlePipeline {
        IdlePipeline::new(IdlePolicy::new("agent-1"))
    }

    fn session_with_repeated_error(session_id: &str) -> SessionData {
        let mut call_msg = Message::assistant("");
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
        let err1 = Message::tool(vec![ContentPart::ToolResult {
            call_id: CompactString::new("c1"),
            output: "permission denied".to_string(),
            is_error: true,
        }]);
        let err2 = Message::tool(vec![ContentPart::ToolResult {
            call_id: CompactString::new("c2"),
            output: "permission denied".to_string(),
            is_error: true,
        }]);
        SessionData {
            session_id: session_id.to_string(),
            agent_id: "agent-1".to_string(),
            messages: vec![call_msg, err1, err2],
            metadata: serde_json::Value::Null,
            created_at_ms: 0,
            updated_at_ms: 1000,
        }
    }

    const VALID_JSON: &str =
        r#"{"insights":[{"text":"Avoid bash in restricted environments","confidence":0.9}]}"#;
    const EMPTY_JSON: &str = r#"{"insights":[]}"#;

    // --- state checks -------------------------------------------------------

    #[test]
    fn starts_idle() {
        assert!(pipeline().is_idle());
    }

    #[test]
    fn empty_sessions_returns_noop_and_stays_idle() {
        let mut p = pipeline();
        let action = p.feed(IdleEvent::Trigger {
            sessions: vec![],
            existing_memories: vec![],
            now_ms: 0,
        });
        assert!(matches!(action, IdleAction::Noop));
        assert!(p.is_idle());
    }

    #[test]
    fn abort_from_any_phase_resets_to_idle() {
        let mut p = pipeline();
        // Trigger → SynthesisPending, then abort.
        p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s1")],
            existing_memories: vec![],
            now_ms: 0,
        });
        assert!(matches!(p.phase, IdlePhase::SynthesisPending { .. }));
        let action = p.feed(IdleEvent::Abort);
        assert!(matches!(action, IdleAction::Aborted));
        assert!(p.is_idle());
    }

    // --- two-phase happy path -----------------------------------------------

    #[test]
    fn trigger_emits_synthesize_insights() {
        let mut p = pipeline();
        let action = p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s1")],
            existing_memories: vec![],
            now_ms: 0,
        });
        assert!(
            matches!(action, IdleAction::SynthesizeInsights { .. }),
            "expected SynthesizeInsights after Trigger"
        );
        assert!(matches!(p.phase, IdlePhase::SynthesisPending { .. }));
    }

    #[test]
    fn synthesis_result_emits_commit_memories() {
        let mut p = pipeline();
        p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s1")],
            existing_memories: vec![],
            now_ms: 5000,
        });
        let action =
            p.feed(IdleEvent::SynthesisResult { content: VALID_JSON.to_string() });
        match action {
            IdleAction::CommitMemories { agent_id, result, run_result } => {
                assert_eq!(agent_id, "agent-1");
                assert_eq!(run_result.sessions_processed, 1);
                assert!(run_result.insights_extracted > 0);
                // Expect at least the synthesized LLM insight in to_add.
                assert!(!result.to_add.is_empty());
            }
            _ => panic!("expected CommitMemories"),
        }
        assert!(matches!(p.phase, IdlePhase::Done { .. }));
    }

    #[test]
    fn synthesized_insights_appear_in_result() {
        let mut p = pipeline();
        p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s1")],
            existing_memories: vec![],
            now_ms: 0,
        });
        let action =
            p.feed(IdleEvent::SynthesisResult { content: VALID_JSON.to_string() });
        if let IdleAction::CommitMemories { result, .. } = action {
            let has_synthesized = result
                .to_add
                .iter()
                .any(|e| e.metadata["kind"] == "synthesized");
            assert!(has_synthesized, "expected at least one synthesized insight");
        }
    }

    #[test]
    fn synthesis_result_without_pending_state_returns_aborted() {
        let mut p = pipeline();
        // Feed SynthesisResult while still in Idle (no Trigger first).
        let action = p.feed(IdleEvent::SynthesisResult { content: VALID_JSON.to_string() });
        assert!(matches!(action, IdleAction::Aborted));
    }

    // --- policy enforcement -------------------------------------------------

    #[test]
    fn respects_max_sessions_per_run() {
        let policy = IdlePolicy { max_sessions_per_run: 1, ..IdlePolicy::new("agent-1") };
        let mut p = IdlePipeline::new(policy);
        let sessions = vec![
            session_with_repeated_error("s1"),
            session_with_repeated_error("s2"),
            session_with_repeated_error("s3"),
        ];
        p.feed(IdleEvent::Trigger { sessions, existing_memories: vec![], now_ms: 0 });
        let action =
            p.feed(IdleEvent::SynthesisResult { content: EMPTY_JSON.to_string() });
        match action {
            IdleAction::CommitMemories { run_result, .. } => {
                assert_eq!(run_result.sessions_processed, 1);
            }
            _ => panic!("expected CommitMemories"),
        }
    }

    // --- lifecycle ----------------------------------------------------------

    #[test]
    fn reset_allows_retriggering() {
        let mut p = pipeline();

        // First cycle.
        p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s1")],
            existing_memories: vec![],
            now_ms: 0,
        });
        p.feed(IdleEvent::SynthesisResult { content: EMPTY_JSON.to_string() });
        assert!(matches!(p.phase, IdlePhase::Done { .. }));

        p.reset();
        assert!(p.is_idle());

        // Second cycle.
        let action = p.feed(IdleEvent::Trigger {
            sessions: vec![session_with_repeated_error("s2")],
            existing_memories: vec![],
            now_ms: 1000,
        });
        assert!(matches!(action, IdleAction::SynthesizeInsights { .. }));
    }
}
