use std::sync::Arc;

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

/// spc_011-C-01: real-BPE-backed counter, the production default. `CharApproxCounter`'s
/// char/4 divisor only holds for English text — on CJK-heavy text it underestimates real BPE
/// counts by 40-70% (its own doc comment claims "~0.5 tokens/char" for CJK, but the
/// implementation applies the same 0.25 tokens/char divisor to every script). This wraps
/// `deepstrike-tokenizer`'s real cl100k BPE tokenizer (previously an orphaned crate — not a
/// workspace member, zero call sites anywhere) and adds a fixed margin on top. That only
/// guarantees a value above the selected `cl100k_base` estimate; it does not prove that the
/// result is conservative for every provider model. Native Anthropic/Gemini token counts take
/// precedence where callers explicitly request them.
pub struct FallbackEstimator {
    tokenizer: deepstrike_tokenizer::Tokenizer,
    /// Multiplier applied on top of the raw BPE count. `1.1` is a starting margin over
    /// `cl100k_base`, not an empirical claim about any provider tokenizer.
    safety_margin: f64,
}

impl FallbackEstimator {
    pub fn new(backend: deepstrike_tokenizer::TokenizerBackend, safety_margin: f64) -> Self {
        Self { tokenizer: deepstrike_tokenizer::Tokenizer::new(backend), safety_margin }
    }
}

impl Default for FallbackEstimator {
    fn default() -> Self {
        Self::new(deepstrike_tokenizer::TokenizerBackend::Cl100k, 1.1)
    }
}

impl TokenCounter for FallbackEstimator {
    fn count(&self, text: &str) -> u32 {
        let raw = self.tokenizer.count(text) as f64;
        ((raw * self.safety_margin).ceil() as u32).max(1)
    }

    fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        // Budget in *raw* BPE tokens so that, after the margin is applied by `count`, the
        // truncated text's reported count still fits within `max_tokens`.
        let raw_budget = ((max_tokens as f64) / self.safety_margin).floor() as u32;
        self.tokenizer.truncate(text, raw_budget)
    }
}

/// Cheaply cloneable token engine shared across the context subsystem.
/// All token counting and truncation goes through this single object —
/// pressure, compression, and render use the same backend.
#[derive(Clone)]
pub struct ContextTokenEngine(Arc<dyn TokenCounter>);

impl ContextTokenEngine {
    /// Deterministic char/4 approximation. Kept as an explicit, opt-in constructor for tests
    /// that pin exact token counts (many do, calibrated to this specific math) — **not** used
    /// by any production call site as of spc_011-C-01; see [`Self::fallback_estimator`].
    pub fn char_approx() -> Self {
        Self(Arc::new(CharApproxCounter))
    }

    /// spc_011-C-01: the production default token engine. Real-BPE-backed (see
    /// [`FallbackEstimator`]), replacing the previous `char_approx()` default that
    /// underestimated CJK-heavy text by 40-70%.
    pub fn fallback_estimator() -> Self {
        Self(Arc::new(FallbackEstimator::default()))
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
            ContentPart::ToolResult { output, .. } => self.count(output),
            // Image/Audio: modality heuristic from ContentPart::estimate_tokens — never
            // treat base64/url payloads as UTF-8 text (that blind-spots compression ρ).
            ContentPart::Image { .. } | ContentPart::Audio { .. } => {
                part.estimate_tokens().unwrap_or(1)
            }
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
    use crate::types::message::{ContentPart, Message};

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

    #[test]
    fn count_image_uses_detail_heuristic_not_one() {
        let e = engine();
        let low = Message::user_multimodal(vec![ContentPart::image_base64_with_detail(
            "abc",
            "image/png",
            "low",
        )]);
        let auto = Message::user_multimodal(vec![ContentPart::image_base64("abc", "image/png")]);
        let high = Message::user_multimodal(vec![ContentPart::image_base64_with_detail(
            "abc",
            "image/png",
            "high",
        )]);
        assert_eq!(e.count_message(&low), 85);
        assert_eq!(e.count_message(&auto), 255);
        assert_eq!(e.count_message(&high), 680);
    }

    /// spc_011-C-01 Red: `CharApproxCounter` (char/4) badly underestimates real BPE token
    /// counts on CJK-heavy text — its own doc comment claims "~0.5 tokens/char" for CJK, but
    /// the implementation divides by 4 (0.25 tokens/char) regardless of script, which is only
    /// correct for English. This reproduces the underestimate against a sample matching this
    /// repo's own actual workload (Chinese spec/status prose), not a synthetic worst case.
    #[test]
    fn char_approx_severely_underestimates_cjk_heavy_text_vs_real_bpe() {
        let sample = "核实 `ContextTokenEngine` 默认使用 `CharApproxCounter`（4 字符≈1 token），\
而 `ContextManager::new()` 明确默认初始化 `ContextTokenEngine::char_approx()`。这就能解释实际观察到的 \
20%～30% 少算问题。这个值直接进入 Context ρ → Snip → Micro → Collapse → Auto → Renewal 决策链路，\
低估会导致压缩没有按时触发，继续 append 下去最终造成 Provider context overflow。";

        let approx = CharApproxCounter.count(sample);
        let real = deepstrike_tokenizer::Tokenizer::new(deepstrike_tokenizer::TokenizerBackend::Cl100k)
            .count(sample);

        let underestimate_pct = 1.0 - (approx as f64 / real as f64);
        assert!(
            underestimate_pct > 0.30,
            "expected char_approx to underestimate real BPE count by >30% on CJK-heavy text, \
             got approx={approx} real={real} ({:.1}%)",
            underestimate_pct * 100.0
        );
    }

    /// The production default (`fallback_estimator`) must not reproduce the CJK underestimate
    /// above: its fixed margin keeps it at or above the selected `cl100k_base` count.
    #[test]
    fn fallback_estimator_does_not_underestimate_cjk_heavy_text() {
        let sample = "核实 `ContextTokenEngine` 默认使用 `CharApproxCounter`（4 字符≈1 token），\
而 `ContextManager::new()` 明确默认初始化 `ContextTokenEngine::char_approx()`。这就能解释实际观察到的 \
20%～30% 少算问题。";

        let e = ContextTokenEngine::fallback_estimator();
        let estimated = e.count(sample);
        let real = deepstrike_tokenizer::Tokenizer::new(deepstrike_tokenizer::TokenizerBackend::Cl100k)
            .count(sample);

        assert!(
            estimated >= real,
            "fallback_estimator margin must stay above its cl100k base \
             (estimated={estimated} real={real})"
        );
    }

    /// The default production engine (`ContextManager::new`) must be the fallback estimator,
    /// not `char_approx` — this is the actual bug: the constructor call site, not the counter
    /// implementation itself (which stays correct as an explicit, deterministic test helper).
    #[test]
    fn context_manager_new_does_not_default_to_char_approx() {
        let cjk = "这是一段包含中文的示例文本，用来验证生产路径默认引擎不再是字符近似计数器。";
        let mgr = crate::context::manager::ContextManager::new(100_000);
        let default_engine_count = mgr.engine.count(cjk);
        let char_approx_count = ContextTokenEngine::char_approx().count(cjk);
        assert_ne!(
            default_engine_count, char_approx_count,
            "ContextManager::new() must not use char_approx as its token engine"
        );
    }

    #[test]
    fn count_audio_uses_decoded_byte_heuristic_not_base64_text() {
        let e = engine();
        // 6400 base64 chars → ~4800 decoded bytes → 4800/1600 = 3 tokens
        let audio =
            Message::user_multimodal(vec![ContentPart::audio("A".repeat(6400), "audio/wav")]);
        assert_eq!(e.count_message(&audio), 3);
        // Must not explode to thousands the way counting base64 as text would.
        assert!(e.count_message(&audio) < 100);
    }
}
