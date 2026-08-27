//! Pure projection helpers from the canonical Kernel Wire contract to host-facing facts.
//!
//! This module intentionally contains no host I/O, clocks, randomness, or runtime state.  The
//! first slice centralises publication-manifest extraction; action projection is added on top of
//! the same boundary in the following TDD cards.

use serde::{Deserialize, Serialize};

use super::driver::PlannedStep;
use super::effect::{
    EffectKind, EffectKindTag, KernelEffect, QueryMemoryEffect, CallProviderEffect,
    ExecuteToolsEffect, RequestApprovalEffect, SpawnTasksEffect, PreemptTasksEffect,
    PersistMemoryEffect, ArchivePageOutEffect, LoadPayloadEffect, EvaluateMilestoneEffect,
    MeasurePromptEffect,
};
use super::scalar::EffectId;

/// The minimal fact a host may append to its event log for a committed step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedEffectRef {
    pub effect_id: EffectId,
    pub kind: EffectKindTag,
}

/// Host-facing action with one canonical effect payload.  Payload structs are the existing wire
/// structs; this first slice centralises selection without inventing a second payload schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalHostAction {
    CallProvider { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: CallProviderEffect },
    ExecuteTools { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: ExecuteToolsEffect },
    RequestApproval { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: RequestApprovalEffect },
    SpawnTasks { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: SpawnTasksEffect },
    PreemptTasks { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: PreemptTasksEffect },
    PersistMemory { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: PersistMemoryEffect },
    QueryMemory { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: QueryMemoryEffect },
    ArchivePageOut { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: ArchivePageOutEffect },
    LoadPayload { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: LoadPayloadEffect },
    EvaluateMilestone { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: EvaluateMilestoneEffect },
    MeasurePrompt { effect_id: EffectId, causation_input_id: super::scalar::InputId, payload: MeasurePromptEffect },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CurrentProjection {
    Idle,
    Action(CanonicalHostAction),
    Terminal(super::terminal::KernelTerminal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionError {
    pub message: String,
}

/// Project one wire effect without touching host state.
pub fn project_effect(effect: &KernelEffect) -> CanonicalHostAction {
    let effect_id = effect.effect_id.clone();
    let causation_input_id = effect.causation_input_id.clone();
    match &effect.effect {
        EffectKind::CallProvider(payload) => CanonicalHostAction::CallProvider { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::ExecuteTools(payload) => CanonicalHostAction::ExecuteTools { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::RequestApproval(payload) => CanonicalHostAction::RequestApproval { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::SpawnTasks(payload) => CanonicalHostAction::SpawnTasks { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::PreemptTasks(payload) => CanonicalHostAction::PreemptTasks { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::PersistMemory(payload) => CanonicalHostAction::PersistMemory { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::QueryMemory(payload) => CanonicalHostAction::QueryMemory { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::ArchivePageOut(payload) => CanonicalHostAction::ArchivePageOut { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::LoadPayload(payload) => CanonicalHostAction::LoadPayload { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::EvaluateMilestone(payload) => CanonicalHostAction::EvaluateMilestone { effect_id, causation_input_id, payload: payload.clone() },
        EffectKind::MeasurePrompt(payload) => CanonicalHostAction::MeasurePrompt { effect_id, causation_input_id, payload: payload.clone() },
    }
}

/// Select the current host action from one committed step.  The step's vector is already the
/// kernel's publication order, so the first effect is the only legal current action.
pub fn project_current_action(step: &PlannedStep) -> Result<CurrentProjection, ProjectionError> {
    project_current_pending_action(step.disposition.terminal(), step.disposition.effects())
}

/// Project the current action from the transaction's already ordered pending-effect view.
/// Ordering is deliberately owned by `KernelTransaction::pending_effects_in_order`; this helper
/// only selects the head and maps it to a canonical action.
pub fn project_current_pending_action<'a, I>(
    terminal: Option<&super::terminal::KernelTerminal>,
    effects: I,
) -> Result<CurrentProjection, ProjectionError>
where
    I: IntoIterator<Item = &'a KernelEffect>,
{
    if let Some(terminal) = terminal {
        return Ok(CurrentProjection::Terminal(terminal.clone()));
    }
    match effects.into_iter().next() {
        Some(effect) => Ok(CurrentProjection::Action(project_effect(effect))),
        None => Ok(CurrentProjection::Idle),
    }
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
    use super::{project_current_action, project_current_pending_action, project_effect, published_effects_manifest, CanonicalHostAction, CurrentProjection};
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

    #[test]
    fn projects_the_first_query_memory_effect_to_a_typed_action() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../tests/fixtures/abi/multi_effect_step.json"
        ))
        .expect("fixture JSON");
        let step: PlannedStep = serde_json::from_value(fixture["planned_step"].clone())
            .expect("planned step");
        let effect = step.disposition.effects().first().expect("first effect");

        let action = project_effect(effect);

        match action {
            CanonicalHostAction::QueryMemory { effect_id, payload, .. } => {
                assert_eq!(effect_id.as_str(), "op-contract:step:9:effect:0");
                assert_eq!(payload.query.text, "past briefs");
                assert_eq!(payload.requested_k, 4);
            }
            other => panic!("expected query_memory action, got {other:?}"),
        }
    }

    #[test]
    fn current_projection_selects_first_effect_and_distinguishes_idle() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../../tests/fixtures/abi/multi_effect_step.json"
        ))
        .expect("fixture JSON");
        let step: PlannedStep = serde_json::from_value(fixture["planned_step"].clone())
            .expect("planned step");

        let projection = project_current_action(&step).expect("projection");
        assert!(matches!(projection, CurrentProjection::Action(CanonicalHostAction::QueryMemory { .. })));

        let idle = PlannedStep {
            root_kind: None,
            focus: None,
            observations: Vec::new(),
            disposition: crate::runtime::kernel::wire::StepDisposition::Effects(Default::default()),
        };
        assert!(matches!(project_current_action(&idle).expect("projection"), CurrentProjection::Idle));

        let ordered = step.disposition.effects().iter();
        assert!(matches!(
            project_current_pending_action(None, ordered).expect("projection"),
            CurrentProjection::Action(CanonicalHostAction::QueryMemory { .. })
        ));
    }
}
