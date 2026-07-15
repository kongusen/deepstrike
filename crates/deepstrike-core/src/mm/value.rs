//! Shared, deterministic value vocabulary for context residency and durable-memory retention.
//!
//! The score deliberately uses fixed-point integer terms. Policy decisions therefore replay
//! byte-for-byte without consulting a wall clock or relying on platform floating-point behavior.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionKind {
    User,
    Feedback,
    Project,
    Reference,
    Skill,
    Artifact,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionFeatures {
    pub pinned: bool,
    pub use_count: u64,
    pub last_used_step: Option<u64>,
    pub current_step: u64,
    pub lease_remaining_steps: Option<u64>,
    pub kind: RetentionKind,
    pub tokens: u32,
    /// Optional confidence in parts-per-million. Knowledge entries use zero; durable memories use
    /// their stored confidence when M3 evaluates retention with this same vocabulary.
    pub confidence_ppm: u32,
    /// TTL/staleness penalty in parts-per-million, supplied by the caller's lifecycle policy.
    pub stale_discount_ppm: u32,
}

/// Higher scores are retained first. `i64::MAX` is reserved for explicit pins.
pub fn deterministic_retention_score(features: RetentionFeatures) -> i64 {
    if features.pinned {
        return i64::MAX;
    }

    // Integer log2(1 + n) is a stable ln(1+n) proxy. One observed use intentionally outweighs
    // pure insertion recency: useful old context must beat fresh but irrelevant context.
    let usage_bucket = if features.use_count == 0 {
        0
    } else {
        64 - features.use_count.saturating_add(1).leading_zeros() as i64
    };
    let usage = usage_bucket.saturating_mul(8_192);

    let recency = features.last_used_step.map_or(0, |last| {
        let age = features.current_step.saturating_sub(last);
        (4_096_u64 / age.saturating_add(1).max(1)) as i64
    });
    let lease = features.lease_remaining_steps.unwrap_or(0).min(1_024) as i64 * 8;
    let kind = match features.kind {
        RetentionKind::User => 1_600,
        RetentionKind::Feedback => 1_800,
        RetentionKind::Project => 1_400,
        RetentionKind::Reference => 1_200,
        RetentionKind::Skill => 3_000,
        RetentionKind::Artifact => 1_700,
        RetentionKind::Other => 0,
    };
    let confidence = i64::from(features.confidence_ppm.min(1_000_000)) / 250;
    let staleness = i64::from(features.stale_discount_ppm.min(1_000_000)) / 125;
    let size = i64::from(features.tokens).saturating_mul(4);

    usage
        .saturating_add(recency)
        .saturating_add(lease)
        .saturating_add(kind)
        .saturating_add(confidence)
        .saturating_sub(staleness)
        .saturating_sub(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features() -> RetentionFeatures {
        RetentionFeatures {
            pinned: false,
            use_count: 0,
            last_used_step: None,
            current_step: 10,
            lease_remaining_steps: None,
            kind: RetentionKind::Reference,
            tokens: 100,
            confidence_ppm: 0,
            stale_discount_ppm: 0,
        }
    }

    #[test]
    fn one_real_use_beats_unreferenced_recency() {
        let mut used = features();
        used.use_count = 1;
        used.last_used_step = Some(1);
        let mut fresh = features();
        fresh.last_used_step = Some(10);
        assert!(deterministic_retention_score(used) > deterministic_retention_score(fresh));
    }

    #[test]
    fn pins_are_absolute_and_staleness_or_size_reduce_value() {
        let base = deterministic_retention_score(features());
        let mut worse = features();
        worse.tokens = 500;
        worse.stale_discount_ppm = 500_000;
        assert!(deterministic_retention_score(worse) < base);
        let mut pinned = worse;
        pinned.pinned = true;
        assert_eq!(deterministic_retention_score(pinned), i64::MAX);
    }
}
