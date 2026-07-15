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
    Text {
        text: String,
    },
    Image {
        /// Remote URL (mutually exclusive with `data`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// Raw base64-encoded image bytes (mutually exclusive with `url`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
        /// MIME type, e.g. `"image/png"`. Required when `data` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        /// OpenAI vision detail level: `"auto"` | `"low"` | `"high"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Audio {
        /// Raw base64-encoded audio bytes.
        data: String,
        /// MIME type, e.g. `"audio/wav"`, `"audio/mp3"`.
        media_type: String,
    },
    ToolResult {
        call_id: CompactString,
        output: String,
        is_error: bool,
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
    /// When `true` the state machine rolls back the current turn on receipt.
    /// Ordinary tool errors leave `is_fatal = false` so the run continues and
    /// the LLM can self-correct. Only set this for writes that mutated shared
    /// state and cannot safely proceed.
    #[serde(default)]
    pub is_fatal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ToolErrorKind>,
    pub token_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: CompactString,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ContentPart {
    /// Modality-aware token estimate for Image/Audio. Returns `None` for text-bearing
    /// parts that must go through a [`crate::context::token_engine::TokenCounter`].
    ///
    /// Image: OpenAI-vision-style tile heuristic (`low=85`, `auto/default=255`, `high=680`).
    /// Audio: `max(1, floor(decoded_bytes / 1600))` where `decoded_bytes ≈ base64_len * 3/4`.
    /// Never treat base64 payloads as UTF-8 text for counting.
    pub fn estimate_tokens(&self) -> Option<u32> {
        match self {
            ContentPart::Image { detail, .. } => Some(match detail.as_deref() {
                Some("low") => 85,
                Some("high") => 680,
                _ => 255,
            }),
            ContentPart::Audio { data, .. } => {
                let decoded_bytes = (data.len() as u64).saturating_mul(3) / 4;
                Some((decoded_bytes / 1600).max(1) as u32)
            }
            ContentPart::Text { .. } | ContentPart::ToolResult { .. } => None,
        }
    }

    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }

    pub fn image_url(url: impl Into<String>) -> Self {
        ContentPart::Image {
            url: Some(url.into()),
            data: None,
            media_type: None,
            detail: None,
        }
    }

    pub fn image_base64(data: impl Into<String>, media_type: impl Into<String>) -> Self {
        ContentPart::Image {
            url: None,
            data: Some(data.into()),
            media_type: Some(media_type.into()),
            detail: None,
        }
    }

    pub fn image_base64_with_detail(
        data: impl Into<String>,
        media_type: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        ContentPart::Image {
            url: None,
            data: Some(data.into()),
            media_type: Some(media_type.into()),
            detail: Some(detail.into()),
        }
    }

    pub fn audio(data: impl Into<String>, media_type: impl Into<String>) -> Self {
        ContentPart::Audio {
            data: data.into(),
            media_type: media_type.into(),
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

    /// Byte/char proxy length. Image/Audio use `estimate_tokens() * 4` so the
    /// 4-chars≈1-token convention stays aligned with [`ContentPart::estimate_tokens`].
    pub fn text_len(&self) -> usize {
        match self {
            Content::Text(s) => s.len(),
            Content::Parts(parts) => parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.len(),
                    ContentPart::ToolResult { output, .. } => output.len(),
                    ContentPart::Image { .. } | ContentPart::Audio { .. } => {
                        p.estimate_tokens().unwrap_or(1) as usize * 4
                    }
                })
                .sum(),
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

    pub fn user_multimodal(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            content: Content::Parts(parts),
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
