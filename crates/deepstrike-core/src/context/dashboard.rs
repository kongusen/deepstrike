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
    #[deprecated(since = "0.2.0", note = "Use TaskState.progress instead")]
    pub goal_progress: String,
    pub error_count: u32,
    pub depth: u32,
    pub interrupt_requested: bool,
    #[deprecated(since = "0.2.0", note = "Use TaskState.plan instead")]
    pub plan: Vec<String>,
    pub event_surface: EventSurface,
    pub knowledge_surface: KnowledgeSurface,
    #[deprecated(since = "0.2.0", note = "Use TaskState.scratchpad instead")]
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
    /// Compact single-block representation for embedding in system_text.
    /// Returns an empty string when all fields are at their default/empty values
    /// so the renderer can skip it entirely on fresh agents.
    pub fn format_compact(&self) -> String {
        let has_progress = !self.goal_progress.is_empty();
        let has_plan = !self.plan.is_empty();
        let has_scratchpad = !self.scratchpad.is_empty();
        let has_questions = !self.knowledge_surface.active_questions.is_empty();
        let has_activity = self.error_count > 0 || self.depth > 0 || self.interrupt_requested;

        if !has_progress && !has_plan && !has_scratchpad && !has_questions && !has_activity {
            return String::new();
        }

        let mut parts: Vec<String> = Vec::new();
        parts.push(format!(
            "[AGENT STATE] rho={:.3} turn={} errors={} interrupt={}",
            self.rho, self.depth, self.error_count, self.interrupt_requested
        ));
        if has_progress {
            parts.push(format!("goal_progress: {}", self.goal_progress));
        }
        if has_plan {
            let plan = self
                .plan
                .iter()
                .enumerate()
                .map(|(i, s)| format!("  {}. {}", i + 1, s))
                .collect::<Vec<_>>()
                .join("\n");
            parts.push(format!("plan:\n{plan}"));
        }
        if has_questions {
            parts.push(format!(
                "active_questions: {}",
                self.knowledge_surface.active_questions.join(", ")
            ));
        }
        if has_scratchpad {
            parts.push(format!("scratchpad: {}", self.scratchpad));
        }
        parts.join("\n")
    }

    pub fn format_message(&self) -> Message {
        let plan_str = if self.plan.is_empty() {
            "(none)".to_string()
        } else {
            self.plan
                .iter()
                .enumerate()
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
