use std::sync::Arc;

use deepstrike_tokenizer::{Tokenizer, TokenizerBackend};

use crate::types::message::{Content, ContentPart, Message};

/// Token counting and truncation interface. Implementations must be
/// deterministic and must never panic on any valid UTF-8 input.
pub trait TokenCounter: Send + Sync {
    /// Count tokens in a UTF-8 string.
    fn count(&self, text: &str) -> u32;

    /// Return the longest prefix of `text` that fits within `max_tokens`.
    /// The returned slice is always a valid UTF-8 prefix of `text`.
    fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str;
}

/// Char-count approximation: 4 chars ≈ 1 token.
/// Used when no real tokeniser is available. More accurate than byte-count
/// for CJK text (3 bytes/char but ~0.5 tokens/char).
pub struct CharApproxCounter;

impl TokenCounter for CharApproxCounter {
    fn count(&self, text: &str) -> u32 {
        (text.chars().count() as u32 / 4).max(1)
    }

    fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        let max_chars = (max_tokens as usize).saturating_mul(4);
        let mut byte_end = text.len(); // default: keep all
        let mut seen = 0usize;
        for (byte_idx, _) in text.char_indices() {
            if seen >= max_chars {
                byte_end = byte_idx;
                break;
            }
            seen += 1;
        }
        &text[..byte_end]
    }
}

/// Real BPE tokeniser backed by tiktoken.
struct TiktokenCounter(Tokenizer);

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> u32 {
        self.0.count(text)
    }

    fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        self.0.truncate(text, max_tokens)
    }
}

/// Cheaply cloneable token engine shared across the context subsystem.
/// All token counting and truncation goes through this single object —
/// pressure, compression, and render use the same backend.
#[derive(Clone)]
pub struct ContextTokenEngine(Arc<dyn TokenCounter>);

impl ContextTokenEngine {
    pub fn char_approx() -> Self {
        Self(Arc::new(CharApproxCounter))
    }

    pub fn cl100k() -> Self {
        Self(Arc::new(TiktokenCounter(Tokenizer::new(
            TokenizerBackend::Cl100k,
        ))))
    }

    pub fn o200k() -> Self {
        Self(Arc::new(TiktokenCounter(Tokenizer::new(
            TokenizerBackend::O200k,
        ))))
    }

    pub fn count(&self, text: &str) -> u32 {
        self.0.count(text)
    }

    pub fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        self.0.truncate(text, max_tokens)
    }

    pub fn token_budget_to_bytes(&self, tokens: u32) -> usize {
        (tokens as usize).saturating_mul(4)
    }

    pub fn count_message(&self, msg: &Message) -> u32 {
        match &msg.content {
            Content::Text(t) => self.count(t),
            Content::Parts(parts) => parts.iter().map(|p| self.count_part(p)).sum(),
        }
    }

    fn count_part(&self, part: &ContentPart) -> u32 {
        match part {
            ContentPart::Text { text } => self.count(text),
            ContentPart::ToolResult { output, .. } => self.count(output.as_str()),
            ContentPart::Image { .. } => 1, // structural token — content is base64/url
            ContentPart::Audio { data, .. } => self.count(data.as_str()),
        }
    }

    /// Truncate a text message to `max_tokens`. Returns the message unchanged
    /// if it fits. Parts messages are never truncated — mangling structured
    /// content produces worse outcomes than a minor token overrun.
    pub fn truncate_message(&self, msg: &Message, max_tokens: u32) -> Message {
        match &msg.content {
            Content::Text(t) => {
                let kept = self.0.truncate(t, max_tokens);
                if kept.len() < t.len() {
                    let mut m = msg.clone();
                    m.content = Content::Text(format!("{}… [truncated]", kept));
                    m.token_count = Some(max_tokens);
                    m
                } else {
                    msg.clone()
                }
            }
            Content::Parts(_) => msg.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Message;

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }

    #[test]
    fn count_nonzero_for_nonempty_text() {
        assert!(engine().count("hello") > 0);
    }

    #[test]
    fn count_is_char_based_not_byte_based() {
        let e = engine();
        // "你好" = 6 bytes, 2 chars → count = max(2/4, 1) = 1
        // "hello" = 5 bytes, 5 chars → count = max(5/4, 1) = 1
        // The key property: count doesn't grow 3× for CJK vs ASCII
        let cjk_count = e.count("你好世界"); // 4 chars
        let ascii_count = e.count("abcd"); // 4 chars (same char count)
        assert_eq!(cjk_count, ascii_count);
    }

    #[test]
    fn truncate_stays_within_budget() {
        let e = engine();
        let text = "a".repeat(1000);
        let kept = e.0.truncate(&text, 10);
        assert!(e.count(kept) <= 10);
    }

    #[test]
    fn truncate_cjk_valid_utf8() {
        let e = engine();
        let text = "你好世界".repeat(100);
        let kept = e.0.truncate(&text, 5);
        assert!(std::str::from_utf8(kept.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_count_le_budget() {
        let e = engine();
        for max in [1u32, 5, 20, 100] {
            let kept =
                e.0.truncate("The quick brown fox jumps over the lazy dog.", max);
            assert!(
                e.count(kept) <= max,
                "max={max} kept_count={}",
                e.count(kept)
            );
        }
    }

    #[test]
    fn truncate_message_appends_suffix_on_cut() {
        let e = engine();
        let msg = Message::user("a".repeat(200));
        let truncated = e.truncate_message(&msg, 5);
        let text = truncated.content.as_text().unwrap();
        assert!(text.ends_with("… [truncated]"), "got: {text}");
    }

    #[test]
    fn truncate_message_unchanged_when_fits() {
        let e = engine();
        let msg = Message::user("hi");
        let out = e.truncate_message(&msg, 1000);
        assert_eq!(out.content.as_text().unwrap(), "hi");
    }
}
