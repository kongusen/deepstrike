use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::durable_content::DurableContent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// The internal runtime message (0.2.67, Q1) — explicitly distinct from `LogicalMessage`
/// (operation-bootstrap Intent), `ProviderMessage` (provider boundary), and
/// `StoredMessageState` (L1 persistent authority, the durable representation of the
/// CanonicalMessageState concept).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreMessage {
    pub role: Role,
    pub content: Content,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
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
    Text {
        text: String,
    },
    Image {
        source: super::durable_content::DurableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Audio {
        source: super::durable_content::DurableSource,
        media_type: String,
    },
    ToolResult {
        call_id: CompactString,
        output: String,
        is_error: bool,
        /// The versioned portable blocks for this result. `output` remains the text projection
        /// consumed by text renderers and bindings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        durable_content: Option<DurableContent>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    Recoverable,
    Fatal,
    GovernanceDenied,
    ProviderFailure,
    Timeout,
    UserInterrupt,
}

/// F5 projection pair (registered in `crate::projection_pairs`, 0.2.66): the wire
/// version is the ABI authority; this is the richer internal semantic vocabulary. The
/// only legal crossing is the driver's exhaustive conversion.
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
    /// The versioned portable blocks supplied by the canonical wire. The state machine keeps
    /// them through history/checkpoint rather than encoding structured output as JSON text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_content: Option<DurableContent>,
    pub is_error: bool,
    /// When `true` the state machine rolls back the current turn on receipt.
    /// Ordinary tool errors leave `is_fatal = false` so the run continues and
    /// the LLM can self-correct. Only set this for writes that mutated shared
    /// state and cannot safely proceed.
    #[serde(default)]
    pub is_fatal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ToolErrorKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: CompactString,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        ContentPart::Image {
            source: super::durable_content::DurableSource::Url { url: url.into() },
            media_type: None,
            detail: None,
        }
    }
}

impl Content {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            _ => None,
        }
    }
}

impl CoreMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn user_multimodal(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: Content::Parts(parts),
            tool_calls: Vec::new(),
        }
    }

    pub fn tool(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::Tool,
            content: Content::Parts(parts),
            tool_calls: Vec::new(),
        }
    }
}
