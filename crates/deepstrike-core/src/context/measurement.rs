//! spc_011-C-02: prompt token measurement — the three-way split the spec's §6.1 invariant
//! ("measure provider-visible input, never underestimate") requires between what a request is
//! *estimated* to cost before it is sent (`PromptMeasurement`, this module), what the provider
//! actually reports back (`ProviderUsage`, Node/Python Host layer — not this crate). This module
//! only defines a preflight fact. The `MeasurePrompt` wire tag is reserved, but A-00R removed its
//! scheduler producer until request fingerprinting and durable replay semantics are defined.

use serde::{Deserialize, Serialize};

/// Where a token count came from — never just a bare `u32`, so a caller can tell "the provider's
/// own count API said so" apart from "we guessed." The provenance remains part of the reserved
/// contract, but no runtime currently persists or consumes this fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeasurementSource {
    /// The provider's own token-counting endpoint answered (e.g. Anthropic
    /// `messages/count_tokens`, OpenAI Responses token counting, Gemini `countTokens`).
    Native { provider: String },
    /// A real BPE tokenizer ran locally, but against a vocabulary that may not be the target
    /// provider's own (e.g. cl100k standing in for a non-OpenAI vendor) — see
    /// `FallbackEstimator` (spc_011-C-01) for the concrete counter this describes.
    LocalExact { tokenizer: String },
    /// No tokenizer ran at all; this is a coarse guess with a generous safety margin.
    Heuristic,
}

/// How much to trust `PromptMeasurement.input_tokens` when deciding whether to compress. Kept
/// separate from `MeasurementSource` — the *reason* a number exists and how much a caller should
/// *lean on it* are different questions (a `LocalExact` cl100k count for an Anthropic request is
/// still nontrivially uncertain, but it is not a bare guess either).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementConfidence {
    Exact,
    HighConfidence,
    LowConfidence,
}

/// A single preflight token-count fact about a candidate render, for a specific provider/model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptMeasurement {
    pub input_tokens: u32,
    pub source: MeasurementSource,
    pub confidence: MeasurementConfidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructs_all_three_measurement_source_variants() {
        let native = MeasurementSource::Native { provider: "anthropic".to_string() };
        let local_exact = MeasurementSource::LocalExact { tokenizer: "cl100k_base".to_string() };
        let heuristic = MeasurementSource::Heuristic;

        assert_ne!(native, local_exact);
        assert_ne!(local_exact, heuristic);
    }

    #[test]
    fn prompt_measurement_round_trips_through_json() {
        let m = PromptMeasurement {
            input_tokens: 1234,
            source: MeasurementSource::Native { provider: "openai".to_string() },
            confidence: MeasurementConfidence::Exact,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: PromptMeasurement = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let raw = r#"{"input_tokens": 10, "source": {"kind": "heuristic"}, "confidence": "exact", "extra": true}"#;
        let result: Result<PromptMeasurement, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "deny_unknown_fields must reject stray keys");
    }
}
