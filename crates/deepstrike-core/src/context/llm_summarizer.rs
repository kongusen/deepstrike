//! LLM-based summarizer for Layer 5 Auto-Compact (Phase 8F).
//!
//! Uses LLM to generate semantic summaries instead of rule-based statistics.
//! This is the only layer in the 5-layer pyramid that requires an API call.

use crate::context::pressure::PressureAction;
use crate::context::summarizer::Summarizer;
use crate::types::message::Message;
use std::time::{SystemTime, UNIX_EPOCH};

/// LLM-based summarizer configuration.
#[derive(Debug, Clone)]
pub struct LLMSummarizer {
    pub model: String,
    pub suppress_questions: bool,
    pub max_summary_tokens: u32,
}

impl LLMSummarizer {
    /// Create a new LLM summarizer.
    pub fn new(model: String, suppress_questions: bool) -> Self {
        Self {
            model,
            suppress_questions,
            max_summary_tokens: 4_000, // Default: 4K tokens for summary
        }
    }

    /// Create a summarizer with custom token limit.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_summary_tokens = max_tokens;
        self
    }

    /// Micro-Compact preprocessing: clear recoverable tool results.
    fn preprocess_messages(&self, messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .map(|msg| {
                if let crate::types::message::Content::Parts(parts) = &msg.content {
                    let new_parts = parts
                        .iter()
                        .map(|part| {
                            if let crate::types::message::ContentPart::ToolResult {
                                call_id, output, ..
                            } = part {
                                // Clear recoverable tool results
                                if output.len() > 200 {
                                    return crate::types::message::ContentPart::Text {
                                        text: format!(
                                            "[tool_result: {} | {} tokens | content omitted]",
                                            call_id,
                                            output.len()
                                        ),
                                    };
                                }
                            }
                            part.clone()
                        })
                        .collect();
                    Message {
                        content: crate::types::message::Content::Parts(new_parts),
                        ..msg.clone()
                    }
                } else {
                    msg.clone()
                }
            })
            .collect()
    }

    /// Build boundary mark.
    fn build_boundary_mark(&self, messages: &[Message]) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        format!(
            "[Boundary: Auto-Compact @ {} UTC]\nCompressed: {} messages → {} tokens\nLast message ID: {}\n",
            now,
            messages.len(),
            messages.iter().map(|m| m.token_count.unwrap_or(0)).sum::<u32>(),
            messages.last().map(|_| "unknown".to_string()).unwrap_or_default()
        )
    }

    /// Extract attachments (recent files, current plan, active skills).
    fn extract_attachments(&self, messages: &[Message]) -> String {
        // Placeholder for attachment extraction
        // In a full implementation, this would:
        // 1. Extract recently read files (sorted by recency)
        // 2. Extract current plan from task_state
        // 3. Extract active skills
        // 4. Fit within token budget

        String::new()
    }

    /// Generate the 9-chapter summary prompt.
    fn build_summary_prompt(&self, compacted: &[Message]) -> String {
        format!(
            r#"
<instructions>
⚠️ 禁止调用任何工具 ⚠️
你是一个对话摘要专家。请将以下对话历史压缩为结构化摘要。

<output_format>
<analysis>
（在此处思考...）
</analysis>

<summary>
1. **Objective**（目标）
   用户的主要目标是什么？

2. **User Messages**（枚举，不是概括）
   - 用户说的第1句话："..."
   - 用户说的第2句话："..."
   - 用户说的第3句话："..."
   （确保每一句话都列出，包括需求变更、新约束、方向调整）

3. **Key Decisions**（关键决策）
   - 决策1：... 理由：...
   - 决策2：... 理由：...

4. **Actions Taken**（已执行操作）
   - 操作1：工具(...）→ 结果
   - 操作2：工具(...）→ 结果

5. **Current State**（当前状态）
   - 工作目录：...
   - 活跃文件：...
   - 环境配置：...

6. **Technical Details**（技术细节）
   - 使用的技术栈：...
   - 关键API/函数：...
   - 重要参数：...

7. **Challenges & Solutions**（挑战与解决方案）
   - 挑战1：... → 解决方案：...
   - 挑战2：... → 解决方案：...

8. **Open Questions**（未解决问题）
   - 问题1：...
   - 问题2：...

9. **Current Work**（最细颗粒度的当前进度）
   具体到文件、函数、行号，让压缩后的Agent能无缝接续：
   - 当前正在修改：`src/foo.rs:123-145`
   - 最后一条用户消息："..."
   - 下一步操作：...
</summary>
</output_format>
</instructions>

Conversation history ({} messages):
</analysis>
"#,
            compacted.len(),
        )
    }
}

impl Summarizer for LLMSummarizer {
    fn summarize(
        &self,
        messages: &[Message],
        _action: PressureAction,
        max_tokens: u32,
    ) -> String {
        // 1. Micro-Compact preprocessing
        let compacted = self.preprocess_messages(messages);

        // 2. Build boundary mark
        let boundary = self.build_boundary_mark(messages);

        // 3. Extract attachments
        let attachments = self.extract_attachments(messages);

        // 4. In a real implementation, this would call an LLM
        // For now, return a placeholder
        format!(
            "{}\n\n[LLM Summary Placeholder]\n\n{}",
            boundary,
            attachments
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_summarizer_preprocesses_tool_results() {
        let summarizer = LLMSummarizer::new("claude-3-5-sonnet-20250214".to_string(), true);

        // Create a message with large tool result
        let parts = vec![crate::types::message::ContentPart::ToolResult {
            call_id: "c1".into(),
            output: "x".repeat(10_000),
            is_error: false,
        }];

        let msg = crate::types::message::Message {
            role: crate::types::message::Role::Tool,
            content: crate::types::message::Content::Parts(parts),
            tool_calls: vec![],
            token_count: Some(10_000),
        };

        let result = summarizer.preprocess_messages(&[msg]);
        assert_eq!(result.len(), 1);
        match &result[0].content {
            crate::types::message::Content::Text(text) => {
                assert!(text.contains("content omitted"));
            }
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn llm_summarizer_builds_boundary_mark() {
        let summarizer = LLMSummarizer::new("test-model".to_string(), true);
        let boundary = summarizer.build_boundary_mark(&[]);
        assert!(boundary.contains("[Boundary: Auto-Compact"));
        assert!(boundary.contains("Compressed: 0 messages"));
    }
}
