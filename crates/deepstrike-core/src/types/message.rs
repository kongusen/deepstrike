use compact_str::CompactString;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Content,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Cached token count — avoids re-counting on every render pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { url: String },
    ToolResult { call_id: CompactString, output: String, is_error: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: CompactString,
    pub name: CompactString,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: CompactString,
    pub output: Content,
    pub is_error: bool,
    pub token_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: CompactString,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn text_len(&self) -> usize {
        match self {
            Content::Text(s) => s.len(),
            Content::Parts(parts) => parts.iter().map(|p| match p {
                ContentPart::Text { text } => text.len(),
                ContentPart::Image { .. } => 340, // ~85 tokens * 4 chars/token estimate
                ContentPart::ToolResult { output, .. } => output.len(),
            }).sum(),
        }
    }
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
            token_count: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
            token_count: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
            token_count: None,
        }
    }

    pub fn tool(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::Tool,
            content: Content::Parts(parts),
            tool_calls: Vec::new(),
            token_count: None,
        }
    }
}
