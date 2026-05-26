use crate::memory::semantic::MemoryEntry;
use crate::types::message::{Content, ContentPart, Message, Role};

pub struct ExtractionPolicy {
    pub min_length: usize,
    pub include_tool_results: bool,
    pub include_questions: bool,
}

impl Default for ExtractionPolicy {
    fn default() -> Self {
        Self {
            min_length: 100,
            include_tool_results: true,
            include_questions: true,
        }
    }
}

pub struct MemoryExtractor {
    pub policy: ExtractionPolicy,
}

impl MemoryExtractor {
    pub fn new(policy: ExtractionPolicy) -> Self {
        Self { policy }
    }

    pub fn extract(&self, messages: &[Message]) -> Vec<MemoryEntry> {
        let mut entries = Vec::new();
        for msg in messages {
            match msg.role {
                Role::Assistant => {
                    if let Some(text) = msg.content.as_text() {
                        if text.len() >= self.policy.min_length {
                            entries.push(entry(text));
                        }
                    }
                }
                Role::User if self.policy.include_questions => {
                    if let Some(text) = msg.content.as_text() {
                        if text.ends_with('?') {
                            entries.push(entry(text));
                        }
                    }
                }
                Role::Tool if self.policy.include_tool_results => {
                    if let Content::Parts(parts) = &msg.content {
                        for part in parts {
                            if let ContentPart::ToolResult {
                                output, is_error, ..
                            } = part
                            {
                                if !is_error && output.len() >= self.policy.min_length {
                                    entries.push(entry(output));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        entries
    }
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        text: text.to_string(),
        score: 0.0,
        metadata: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_long_assistant_messages() {
        let extractor = MemoryExtractor::new(ExtractionPolicy::default());
        let msg = Message::assistant("a".repeat(101));
        let entries = extractor.extract(&[msg]);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn extracts_user_questions() {
        let extractor = MemoryExtractor::new(ExtractionPolicy::default());
        let msg = Message::user("What is the answer?");
        let entries = extractor.extract(&[msg]);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn skips_short_assistant_messages() {
        let extractor = MemoryExtractor::new(ExtractionPolicy::default());
        let msg = Message::assistant("short");
        let entries = extractor.extract(&[msg]);
        assert!(entries.is_empty());
    }
}
