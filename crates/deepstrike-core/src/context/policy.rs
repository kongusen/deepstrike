use super::config::ContextConfig;

pub const CONTEXT_POLICY_VERSION: u32 = 1;
pub const PPM_SCALE: u32 = 1_000_000;

/// Stable public context policy. Algorithm-only compactor knobs intentionally stay in
/// `ContextConfig` and are not part of this replay contract.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicyV1 {
    pub version: u32,
    pub pressure_thresholds_ppm: PressureThresholdsPpm,
    pub target_after_compress_ppm: u32,
    pub preserve_recent_turns: u32,
    pub renewal_carryover_ppm: u32,
    pub collapse_old_assistant_narration: bool,
    pub idle_micro_compact_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureThresholdsPpm {
    pub snip: u32,
    pub micro: u32,
    pub collapse: u32,
    pub auto: u32,
    pub renewal: u32,
}

impl ContextPolicyV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != CONTEXT_POLICY_VERSION {
            return Err(format!(
                "unsupported context policy version {}; expected {}",
                self.version, CONTEXT_POLICY_VERSION
            ));
        }
        let p = &self.pressure_thresholds_ppm;
        if [p.snip, p.micro, p.collapse, p.auto, p.renewal]
            .into_iter()
            .any(|value| value > PPM_SCALE)
        {
            return Err("context pressure thresholds must be between 0 and 1000000 ppm".into());
        }
        if !(p.snip < p.micro && p.micro < p.collapse && p.collapse < p.auto && p.auto < p.renewal)
        {
            return Err(
                "context pressure thresholds must satisfy snip < micro < collapse < auto < renewal"
                    .into(),
            );
        }
        if self.target_after_compress_ppm >= p.snip {
            return Err("target_after_compress_ppm must be lower than the snip threshold".into());
        }
        if self.renewal_carryover_ppm > PPM_SCALE {
            return Err("renewal_carryover_ppm must be at most 1000000".into());
        }
        if self.preserve_recent_turns == 0 {
            return Err("preserve_recent_turns must be greater than zero".into());
        }
        Ok(())
    }

    pub(crate) fn apply_to(&self, config: &mut ContextConfig) {
        let p = &self.pressure_thresholds_ppm;
        config.snip_threshold = ppm_ratio(p.snip);
        config.micro_threshold = ppm_ratio(p.micro);
        config.collapse_threshold = ppm_ratio(p.collapse);
        config.auto_threshold = ppm_ratio(p.auto);
        config.renewal_threshold = ppm_ratio(p.renewal);
        config.target_after_compress = ppm_ratio(self.target_after_compress_ppm);
        config.preserve_recent_turns = self.preserve_recent_turns as usize;
        config.carryover_ratio = ppm_ratio(self.renewal_carryover_ppm);
        config.collapse_assistant_narration = self.collapse_old_assistant_narration;
        config.micro_compact_idle_minutes = self.idle_micro_compact_minutes;
    }
}

fn ppm_ratio(value: u32) -> f64 {
    value as f64 / PPM_SCALE as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> ContextPolicyV1 {
        ContextPolicyV1 {
            version: 1,
            pressure_thresholds_ppm: PressureThresholdsPpm {
                snip: 700_000,
                micro: 800_000,
                collapse: 900_000,
                auto: 950_000,
                renewal: 980_000,
            },
            target_after_compress_ppm: 650_000,
            preserve_recent_turns: 2,
            renewal_carryover_ppm: 50_000,
            collapse_old_assistant_narration: true,
            idle_micro_compact_minutes: 60,
        }
    }

    #[test]
    fn applies_the_stable_integer_wire_to_internal_config() {
        let mut config = ContextConfig::default();
        policy().apply_to(&mut config);
        assert_eq!(config.snip_threshold, 0.7);
        assert_eq!(config.target_after_compress, 0.65);
        assert_eq!(config.carryover_ratio, 0.05);
        assert_eq!(config.preserve_recent_turns, 2);
    }

    #[test]
    fn validates_the_policy_as_one_atomic_unit() {
        let mut invalid = policy();
        invalid.pressure_thresholds_ppm.micro = 699_999;
        assert!(invalid.validate().unwrap_err().contains("snip < micro"));
        let mut invalid = policy();
        invalid.target_after_compress_ppm = 700_000;
        assert!(invalid.validate().unwrap_err().contains("lower than"));
    }
}
