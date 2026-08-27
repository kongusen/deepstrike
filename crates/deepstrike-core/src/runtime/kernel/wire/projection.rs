//! Pure projection helpers from the canonical Kernel Wire contract to host-facing facts.
//!
//! This module intentionally contains no host I/O, clocks, randomness, or runtime state.  The
//! first slice centralises publication-manifest extraction; action projection is added on top of
//! the same boundary in the following TDD cards.

use serde::{Deserialize, Serialize};

use super::driver::PlannedStep;
use super::effect::{EffectKindTag, KernelEffect};
use super::scalar::EffectId;

/// The minimal fact a host may append to its event log for a committed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedEffectRef {
    pub effect_id: EffectId,
    pub kind: EffectKindTag,
}

/// Extract effects in the step's publication/mint order.
///
/// A terminal step publishes no effects.  The function deliberately preserves the vector order
/// from `PlannedStep`; re-sorting by map keys here would reintroduce the step:10-before-step:9
/// bug that this projection boundary exists to prevent.
pub fn published_effects_manifest(step: &PlannedStep) -> Vec<PublishedEffectRef> {
    step.disposition
        .effects()
        .iter()
        .map(effect_ref)
        .collect()
}

fn effect_ref(effect: &KernelEffect) -> PublishedEffectRef {
    PublishedEffectRef {
        effect_id: effect.effect_id.clone(),
        kind: effect.tag(),
    }
}

#[cfg(test)]
mod tests {
    use super::published_effects_manifest;
    use crate::runtime::kernel::wire::{EffectKindTag, PlannedStep};

    #[test]
    fn manifest_preserves_multi_effect_publication_order() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../tests/fixtures/abi/multi_effect_step.json"
        ))
        .expect("fixture JSON");
        let step: PlannedStep = serde_json::from_value(fixture["planned_step"].clone())
            .expect("planned step");

        let manifest = published_effects_manifest(&step);

        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest[0].effect_id.as_str(), "op-contract:step:9:effect:0");
        assert_eq!(manifest[0].kind, EffectKindTag::QueryMemory);
        assert_eq!(manifest[1].effect_id.as_str(), "op-contract:step:9:effect:1");
        assert_eq!(manifest[1].kind, EffectKindTag::ExecuteTools);
    }

    #[test]
    fn terminal_step_has_empty_manifest() {
        let step = PlannedStep {
            root_kind: None,
            focus: None,
            observations: Vec::new(),
            disposition: crate::runtime::kernel::wire::StepDisposition::Terminal(
                crate::runtime::kernel::wire::TerminalDisposition {
                    terminal: crate::runtime::kernel::wire::KernelTerminal::Cancelled(
                        crate::runtime::kernel::wire::CancelledTerminal {
                            reason: crate::runtime::kernel::wire::CancellationReason::HostShutdown,
                            usage: crate::runtime::kernel::wire::UsageReport {
                                input_tokens: crate::runtime::kernel::wire::WireU64::ZERO,
                                output_tokens: crate::runtime::kernel::wire::WireU64::ZERO,
                                turns: 0,
                                cached_input_tokens: None,
                            },
                        },
                    ),
                },
            ),
        };

        assert!(published_effects_manifest(&step).is_empty());
    }
}
