//! UTF-8-safe text truncation for context render and compression.
//!
//! All byte-index cuts must land on `char` boundaries — slicing mid-scalar
//! panics in debug builds and produces invalid strings in release.

/// Return the longest prefix of `text` with at most `max_bytes` UTF-8 bytes,
/// never splitting a scalar value.
pub fn truncate_bytes_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Truncate `text` to at most `max_bytes` bytes on a char boundary and append `suffix`.
pub fn truncate_with_suffix(text: &str, max_bytes: usize, suffix: &str) -> String {
    let prefix = truncate_bytes_at_char_boundary(text, max_bytes);
    format!("{prefix}{suffix}")
}

/// Proportional byte budget for render-time truncation: keep `remaining` of `total`
/// estimated tokens from a message whose content is `text` bytes long.
pub fn proportional_byte_keep(text: &str, total_tokens: u32, remaining: u32) -> usize {
    if total_tokens == 0 || remaining == 0 {
        return 0;
    }
    let keep = (text.len() * remaining as usize / total_tokens as usize).max(1);
    keep.min(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundary_for_cjk() {
        // "你好世界" = 12 bytes; cut at byte 5 would split 好 (3-byte char).
        let text = "你好世界";
        assert_eq!(truncate_bytes_at_char_boundary(text, 5), "你");
        assert_eq!(truncate_bytes_at_char_boundary(text, 12), text);
    }

    #[test]
    fn truncate_with_suffix_on_cjk() {
        let text = "你好世界";
        let out = truncate_with_suffix(text, 5, "…");
        assert_eq!(out, "你…");
    }

    #[test]
    fn proportional_keep_never_exceeds_len() {
        let text = "你好";
        assert_eq!(proportional_byte_keep(text, 10, 10), 6);
        assert!(proportional_byte_keep(text, 10, 3) <= text.len());
    }
}
