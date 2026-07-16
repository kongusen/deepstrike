//! Shared lexical vocabulary for deterministic in-kernel value scoring.
//!
//! Compression utility (goal overlap, directive dependency) and memory curation
//! (fuzzy dedupe) must speak ONE term vocabulary — and it must discriminate for
//! CJK text, where there is no whitespace between words: per-character unigrams
//! make every Chinese unit overlap every Chinese goal, and whitespace tokens
//! make a whole Chinese sentence a single token. Terms here are lowercased word
//! runs plus Han character bigrams, matching the host-side recall rankers
//! (node `memory/ranking.ts`, python `memory/ranking.py`) so knowledge
//! residency, compression, and memory recall share one notion of "relevant".

use std::collections::BTreeSet;

/// Deterministic term set of `text`.
///
/// - ASCII word runs (`[a-z0-9_\-/.:]`, lowercased) of more than one char are
///   one term each — call ids, paths, and URLs stay whole.
/// - Non-ASCII alphanumeric runs of more than one char are one term each; a
///   lone non-ASCII char is kept as its own term.
/// - Runs containing Han chars additionally emit every adjacent-char bigram,
///   the standard whitespace-free segmentation proxy.
///
/// Punctuation (ASCII and fullwidth alike) and whitespace never become terms.
pub fn terms(text: &str) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    let mut ascii_run = String::new();
    let mut wide_run: Vec<char> = Vec::new();
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '/' | '.' | ':') {
            flush_wide(&mut wide_run, &mut output);
            ascii_run.push(character);
        } else if !character.is_ascii() && character.is_alphanumeric() {
            flush_ascii(&mut ascii_run, &mut output);
            wide_run.push(character);
        } else {
            flush_ascii(&mut ascii_run, &mut output);
            flush_wide(&mut wide_run, &mut output);
        }
    }
    flush_ascii(&mut ascii_run, &mut output);
    flush_wide(&mut wide_run, &mut output);
    output
}

/// Number of terms shared by both sets.
pub fn overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> u32 {
    left.intersection(right).count() as u32
}

/// Term-set Jaccard similarity over the shared vocabulary.
///
/// Inputs whose combined vocabulary is empty score 0.0 — no lexical evidence is
/// treated as "not a duplicate", so degenerate content fails open (both records
/// are kept) rather than merging on nothing.
pub fn jaccard(left: &str, right: &str) -> f64 {
    let left = terms(left);
    let right = terms(right);
    let union = left.union(&right).count();
    if union == 0 {
        return 0.0;
    }
    left.intersection(&right).count() as f64 / union as f64
}

fn flush_ascii(run: &mut String, output: &mut BTreeSet<String>) {
    // All-ASCII, so byte length equals char count.
    if run.len() > 1 {
        output.insert(std::mem::take(run));
    } else {
        run.clear();
    }
}

fn flush_wide(run: &mut Vec<char>, output: &mut BTreeSet<String>) {
    match run.len() {
        0 => return,
        1 => {
            output.insert(run[0].to_string());
        }
        _ => {
            output.insert(run.iter().collect());
            if run.iter().copied().any(is_han) {
                for pair in run.windows(2) {
                    output.insert(pair.iter().collect());
                }
            }
        }
    }
    run.clear();
}

/// Han ranges mirrored from the host rankers (BMP unified + ext A, compat, ext B+).
fn is_han(character: char) -> bool {
    matches!(
        u32::from(character),
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_words_paths_and_ids_stay_whole() {
        let output = terms("Read src/main.rs via call_abc-123, then STOP.");
        assert!(output.contains("read"));
        assert!(output.contains("src/main.rs"));
        assert!(output.contains("call_abc-123"));
        // Single ASCII chars never become terms; punctuation is stripped.
        assert!(!output.contains("a"));
        assert!(!output.contains(","));
    }

    #[test]
    fn han_runs_emit_bigrams_not_unigrams() {
        let output = terms("实现用户登录");
        assert!(output.contains("实现"));
        assert!(output.contains("用户"));
        assert!(output.contains("户登"));
        assert!(output.contains("实现用户登录"));
        assert!(!output.contains("实"), "unigram noise must be gone");
    }

    #[test]
    fn chinese_goal_overlap_discriminates() {
        let goal = terms("实现用户登录功能");
        let on_topic = terms("已完成用户登录表单");
        let off_topic = terms("今天天气很好我们去公园");
        assert!(overlap_count(&goal, &on_topic) >= 2);
        assert_eq!(overlap_count(&goal, &off_topic), 0);
    }

    #[test]
    fn fullwidth_punctuation_is_not_a_term() {
        let output = terms("完成了。下一步：测试！");
        assert!(!output.contains("。"));
        assert!(!output.contains("："));
        assert!(output.contains("完成"));
    }

    #[test]
    fn lone_wide_char_is_kept() {
        assert!(terms("改 a").contains("改"));
    }

    #[test]
    fn mixed_ascii_and_han_split_into_both_vocabularies() {
        let output = terms("部署v2服务到prod环境");
        assert!(output.contains("v2"));
        assert!(output.contains("prod"));
        assert!(output.contains("部署"));
        assert!(output.contains("服务"));
        assert!(output.contains("环境"));
    }

    #[test]
    fn jaccard_near_duplicate_chinese_scores_high() {
        let near = jaccard("用户偏好深色模式界面", "用户偏好浅色模式界面");
        let unrelated = jaccard("用户偏好深色模式界面", "周五之前完成部署上线");
        assert!(near > 0.5, "near-duplicates must be detectable: {near}");
        assert!(unrelated < 0.2, "unrelated must stay low: {unrelated}");
    }

    #[test]
    fn jaccard_identical_english_is_one_and_empty_vocabulary_is_zero() {
        assert_eq!(jaccard("prefer cargo nextest", "prefer cargo nextest"), 1.0);
        assert_eq!(jaccard("", ""), 0.0);
    }

    #[test]
    fn non_han_scripts_keep_whole_words_without_bigrams() {
        let output = terms("привет мир");
        assert!(output.contains("привет"));
        assert!(output.contains("мир"));
        assert!(!output.contains("пр"));
    }
}
