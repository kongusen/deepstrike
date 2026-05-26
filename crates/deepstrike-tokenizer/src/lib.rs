//! # DeepStrike Tokenizer
//!
//! Multi-model token counting engine.

use tiktoken_rs::{CoreBPE, cl100k_base, o200k_base};

/// Supported tokenizer backends.
pub enum TokenizerBackend {
    /// cl100k_base — GPT-4, GPT-3.5
    Cl100k,
    /// o200k_base — GPT-4o
    O200k,
}

/// Token counter with cached BPE instance.
pub struct Tokenizer {
    bpe: CoreBPE,
}

impl Tokenizer {
    pub fn new(backend: TokenizerBackend) -> Self {
        let bpe = match backend {
            TokenizerBackend::Cl100k => cl100k_base().expect("failed to load cl100k_base"),
            TokenizerBackend::O200k => o200k_base().expect("failed to load o200k_base"),
        };
        Self { bpe }
    }

    /// Count tokens in text.
    pub fn count(&self, text: &str) -> u32 {
        self.bpe.encode_ordinary(text).len() as u32
    }

    /// Count tokens for multiple texts.
    pub fn count_batch(&self, texts: &[&str]) -> Vec<u32> {
        texts.iter().map(|t| self.count(t)).collect()
    }

    /// Truncate text to fit within a token budget.
    /// Returns the longest prefix that fits.
    pub fn truncate<'a>(&self, text: &'a str, max_tokens: u32) -> &'a str {
        let tokens = self.bpe.encode_ordinary(text);
        if tokens.len() as u32 <= max_tokens {
            return text;
        }

        // Binary search for the right byte boundary
        let target = &tokens[..max_tokens as usize];
        let decoded = self.bpe.decode(target.to_vec()).unwrap_or_default();
        let byte_len = decoded.len().min(text.len());

        // Ensure we land on a valid UTF-8 boundary
        let mut end = byte_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }
}

/// Quick helper: count tokens with cl100k_base.
pub fn count_tokens(text: &str) -> u32 {
    // Note: in production, cache the Tokenizer instance
    let t = Tokenizer::new(TokenizerBackend::Cl100k);
    t.count(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tokens() {
        let t = Tokenizer::new(TokenizerBackend::Cl100k);
        let count = t.count("Hello, world!");
        assert!(count > 0);
        assert!(count < 10);
    }

    #[test]
    fn truncate_respects_budget() {
        let t = Tokenizer::new(TokenizerBackend::Cl100k);
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(100);
        let truncated = t.truncate(&text, 10);
        assert!(t.count(truncated) <= 10);
        assert!(!truncated.is_empty());
    }

    #[test]
    fn batch_counting() {
        let t = Tokenizer::new(TokenizerBackend::Cl100k);
        let counts = t.count_batch(&["hello", "world", "foo bar baz"]);
        assert_eq!(counts.len(), 3);
        assert!(counts.iter().all(|&c| c > 0));
    }
}
