use crate::types::message::Message;

#[derive(Debug, Clone, Default)]
pub struct EventSurface {
    pub pending_events: Vec<serde_json::Value>,
    pub active_risks: Vec<serde_json::Value>,
    pub recent_event_decisions: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Default)]
pub struct KnowledgeSurface {
    pub active_questions: Vec<String>,
    pub evidence_packs: Vec<serde_json::Value>,
    pub citations: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub rho: f64,
    pub token_budget: u32,
    pub goal_progress: String,
    pub error_count: u32,
    pub depth: u32,
    pub interrupt_requested: bool,
    pub plan: Vec<String>,
    pub event_surface: EventSurface,
    pub knowledge_surface: KnowledgeSurface,
    pub scratchpad: String,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            rho: 0.0,
            token_budget: 0,
            goal_progress: String::new(),
            error_count: 0,
            depth: 0,
            interrupt_requested: false,
            plan: Vec::new(),
            event_surface: EventSurface::default(),
            knowledge_surface: KnowledgeSurface::default(),
            scratchpad: String::new(),
        }
    }
}

impl Dashboard {
    /// Cheap token estimate without full string rendering.
    /// Used by total_tokens() hot path.
    pub fn token_estimate(&self) -> u32 {
        let base = 20u32; // fixed fields
        let plan_chars: usize = self.plan.iter().map(|s| s.len()).sum();
        let q_chars: usize = self.knowledge_surface.active_questions.iter().map(|s| s.len()).sum();
        let dynamic = (self.goal_progress.len() + self.scratchpad.len() + plan_chars + q_chars) / 4;
        base + dynamic as u32
    }

    pub fn format_message(&self) -> Message {
        let plan_str = if self.plan.is_empty() {
            "(none)".to_string()
        } else {
            self.plan.iter().enumerate()
                .map(|(i, s)| format!("  {}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let questions = if self.knowledge_surface.active_questions.is_empty() {
            "(none)".to_string()
        } else {
            self.knowledge_surface.active_questions.join(", ")
        };

        let content = format!(
            "[DASHBOARD]\nrho={:.3} budget={} errors={} depth={} interrupt={}\ngoal_progress: {}\nplan:\n{}\nactive_questions: {}\nscratchpad: {}",
            self.rho,
            self.token_budget,
            self.error_count,
            self.depth,
            self.interrupt_requested,
            self.goal_progress,
            plan_str,
            questions,
            self.scratchpad,
        );

        Message::system(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Role;

    #[test]
    fn format_message_produces_system_message() {
        let d = Dashboard::default();
        let msg = d.format_message();
        assert_eq!(msg.role, Role::System);
        if let crate::types::message::Content::Text(ref t) = msg.content {
            assert!(t.contains("[DASHBOARD]"));
        }
    }
}
