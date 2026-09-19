//! Canonical Context execution contracts.
//!
//! Context has three deliberately different representations:
//!
//! * [`ContextState`] is the semantic state from which a context is selected;
//! * [`ContextPlan`] is the admitted runtime optimisation decision;
//! * [`ContextExecutionInput`] is the frozen identity of one provider execution input.
//!
//! Provider request bytes remain host evidence. None of these objects is a provider request
//! serializer, and none of them carries token projections as semantic message fields.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use thiserror::Error;

use crate::evolution::ContentDigest;
use crate::runtime::kernel::wire::record::canonical_bytes;
use crate::types::message::CoreMessage;

use super::partitions::ContextPartitions;
use super::renderer::InternalRenderedContext;

pub const CONTEXT_SCHEMA: &str = "context/v1";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContextContractError {
    #[error("context canonical value could not be serialized: {0}")]
    Canonical(String),
    #[error("context {kind} digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        kind: &'static str,
        expected: ContentDigest,
        actual: ContentDigest,
    },
    #[error("context plan references an entry that is not in state: {0}")]
    UnknownEntry(String),
    #[error("context plan state generation mismatch: expected {expected}, got {actual}")]
    GenerationMismatch { expected: u64, actual: u64 },
    #[error("context execution input field mismatch: {field}")]
    FieldMismatch { field: &'static str },
    #[error("unsupported context schema: {0}")]
    UnsupportedSchema(String),
    #[error("duplicate context entry or selection: {0}")]
    DuplicateEntry(String),
}

fn digest<T: Serialize>(value: &T) -> Result<ContentDigest, ContextContractError> {
    let bytes = canonical_bytes(value)
        .map_err(|error| ContextContractError::Canonical(error.to_string()))?;
    Ok(ContentDigest::from_bytes(bytes.as_slice()))
}

pub(crate) fn message_digest(
    message: &CoreMessage,
    handles: &crate::mm::handle::HandleTable,
) -> Result<ContentDigest, ContextContractError> {
    digest(&message_material(message, handles))
}

pub(crate) fn message_material(
    message: &CoreMessage,
    handles: &crate::mm::handle::HandleTable,
) -> serde_json::Value {
    use crate::types::durable_content::DurableContent;
    use crate::types::message::{Content, ContentPart};
    // Checkpoints may rebuild text results with explicit durable blocks, and page-out replaces
    // their resident body with a preview. These are representations of the same semantic object.
    let blocks = match &message.content {
        Content::Text(text) => vec![serde_json::json!({"type": "text", "text": text})],
        Content::Parts(parts) => parts.iter().map(|part| match part {
            ContentPart::ToolResult { call_id, output, is_error, durable_content } => {
                let reference = handles.all().iter().find(|h| h.source.as_deref() == Some(call_id.as_str()))
                    .filter(|h| h.residency.digest().is_some());
                match reference {
                    Some(handle) => serde_json::json!({
                        "type": "tool_result", "call_id": call_id, "is_error": is_error,
                        "reference": {"payload_ref": handle.residency.payload_ref(), "digest": handle.residency.digest()},
                    }),
                    None => serde_json::json!({
                        "type": "tool_result", "call_id": call_id, "is_error": is_error,
                        "content": durable_content.clone().unwrap_or_else(|| DurableContent::text(output)),
                    }),
                }
            }
            _ => serde_json::to_value(part).expect("content part is serializable"),
        }).collect(),
    };
    serde_json::json!([&message.role, blocks, &message.tool_calls])
}

fn verify_schema(schema: &str) -> Result<(), ContextContractError> {
    if schema != CONTEXT_SCHEMA {
        return Err(ContextContractError::UnsupportedSchema(schema.to_string()));
    }
    Ok(())
}

/// The semantic area from which an entry came. This is provenance, not a provider role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEntrySource {
    System,
    Knowledge,
    History,
    State,
    Signal,
}

/// A stable reference to one addressable semantic item in ContextState.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextEntryRef {
    pub entry_id: String,
    pub content_digest: ContentDigest,
    pub source: ContextEntrySource,
    pub ordinal: u32,
}

/// Canonical Context state used to prepare one execution input.
///
/// Bodies remain in the kernel's existing semantic state/checkpoint surfaces. This object is the
/// explicit, content-addressed index of that state; measurements and provider rendering are not
/// part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextState {
    pub schema: String,
    pub generation: u64,
    pub system: Vec<ContextEntryRef>,
    pub knowledge: Vec<ContextEntryRef>,
    pub history: Vec<ContextEntryRef>,
    pub state: Vec<ContextEntryRef>,
    pub task_state: ContentDigest,
    pub signals: Vec<ContentDigest>,
    pub digest: ContentDigest,
}

#[derive(Debug, Serialize)]
struct ContextStateBody<'a> {
    schema: &'a str,
    generation: u64,
    system: &'a [ContextEntryRef],
    knowledge: &'a [ContextEntryRef],
    history: &'a [ContextEntryRef],
    state: &'a [ContextEntryRef],
    task_state: &'a ContentDigest,
    signals: &'a [ContentDigest],
}

impl ContextState {
    pub fn from_partitions(
        partitions: &ContextPartitions,
        generation: u64,
    ) -> Result<Self, ContextContractError> {
        Self::from_partitions_with_handles(
            partitions,
            generation,
            &crate::mm::handle::HandleTable::new(),
        )
    }

    pub fn from_partitions_with_handles(
        partitions: &ContextPartitions,
        generation: u64,
        handles: &crate::mm::handle::HandleTable,
    ) -> Result<Self, ContextContractError> {
        let refs_for_messages = |source: ContextEntrySource, messages: &[CoreMessage]| {
            messages
                .iter()
                .enumerate()
                .map(|(ordinal, message)| {
                    let content_digest = message_digest(message, handles)?;
                    Ok(ContextEntryRef {
                        entry_id: format!("{}:{ordinal}:{}", source_label(source), content_digest),
                        content_digest,
                        source,
                        ordinal: ordinal as u32,
                    })
                })
                .collect::<Result<Vec<_>, ContextContractError>>()
        };

        let system = refs_for_messages(ContextEntrySource::System, &partitions.system.messages)?;
        let knowledge = partitions
            .knowledge
            .entries
            .iter()
            .enumerate()
            .map(|(ordinal, entry)| {
                let content_digest = message_digest(&entry.message, handles)?;
                Ok(ContextEntryRef {
                    entry_id: format!(
                        "knowledge:{ordinal}:{}:{}",
                        entry.key.as_deref().unwrap_or("unkeyed"),
                        content_digest
                    ),
                    content_digest,
                    source: ContextEntrySource::Knowledge,
                    ordinal: ordinal as u32,
                })
            })
            .collect::<Result<Vec<_>, ContextContractError>>()?;
        let history = refs_for_messages(ContextEntrySource::History, &partitions.history.messages)?;
        let task_state = digest(&partitions.task_state)?;
        let mut state = Vec::with_capacity(1 + partitions.signals.len());
        state.push(ContextEntryRef {
            entry_id: format!("state:task_state:{task_state}"),
            content_digest: task_state.clone(),
            source: ContextEntrySource::State,
            ordinal: 0,
        });
        let signals = partitions
            .signals
            .iter()
            .enumerate()
            .map(|(ordinal, signal)| {
                let content_digest = digest(signal)?;
                state.push(ContextEntryRef {
                    entry_id: format!("signal:{ordinal}:{content_digest}"),
                    content_digest: content_digest.clone(),
                    source: ContextEntrySource::Signal,
                    ordinal: ordinal as u32,
                });
                Ok(content_digest)
            })
            .collect::<Result<Vec<_>, ContextContractError>>()?;
        let unsigned = Self {
            schema: CONTEXT_SCHEMA.to_string(),
            generation,
            system,
            knowledge,
            history,
            state,
            task_state,
            signals,
            digest: ContentDigest::from_bytes(b"context-state-placeholder"),
        };
        let digest = digest(&ContextStateBody::from(&unsigned))?;
        Ok(Self { digest, ..unsigned })
    }

    pub fn verify_digest(&self) -> Result<(), ContextContractError> {
        verify_schema(&self.schema)?;
        let mut ids = HashSet::new();
        for entry in self
            .system
            .iter()
            .chain(&self.knowledge)
            .chain(&self.history)
            .chain(&self.state)
        {
            if !ids.insert(&entry.entry_id) {
                return Err(ContextContractError::DuplicateEntry(entry.entry_id.clone()));
            }
        }
        let expected = digest(&ContextStateBody::from(self))?;
        if expected != self.digest {
            return Err(ContextContractError::DigestMismatch {
                kind: "state",
                expected,
                actual: self.digest.clone(),
            });
        }
        Ok(())
    }

    pub fn contains_entry(&self, entry_id: &str) -> bool {
        self.system
            .iter()
            .chain(self.knowledge.iter())
            .chain(self.history.iter())
            .chain(self.state.iter())
            .any(|entry| entry.entry_id == entry_id)
    }
}

impl<'a> From<&'a ContextState> for ContextStateBody<'a> {
    fn from(value: &'a ContextState) -> Self {
        Self {
            schema: &value.schema,
            generation: value.generation,
            system: &value.system,
            knowledge: &value.knowledge,
            history: &value.history,
            state: &value.state,
            task_state: &value.task_state,
            signals: &value.signals,
        }
    }
}

fn source_label(source: ContextEntrySource) -> &'static str {
    match source {
        ContextEntrySource::System => "system",
        ContextEntrySource::Knowledge => "knowledge",
        ContextEntrySource::History => "history",
        ContextEntrySource::State => "state",
        ContextEntrySource::Signal => "signal",
    }
}

/// The action the runtime admitted for one Context entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPlanAction {
    Include,
    Excerpt,
    Collapse,
    PageOut,
    Omit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSelection {
    pub entry_id: String,
    pub action: ContextPlanAction,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachePrefixBoundary {
    pub digest: ContentDigest,
    pub entries: u32,
}

/// A deterministic runtime decision. It is not a mutable copy of ContextState.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPlan {
    pub schema: String,
    pub plan_id: ContentDigest,
    pub operation_id: String,
    pub step_id: String,
    pub state_digest: ContentDigest,
    pub state_generation: u64,
    pub runtime_inputs: ContentDigest,
    pub policy_digest: ContentDigest,
    pub provider_profile_digest: ContentDigest,
    pub measurement_fingerprints: Vec<ContentDigest>,
    pub selections: Vec<ContextSelection>,
    pub input_budget_tokens: u32,
    pub projected_tokens: u32,
    pub pressure_ppm: u32,
    pub cache_prefix: Option<CachePrefixBoundary>,
}

#[derive(Debug, Serialize)]
struct ContextPlanBody<'a> {
    schema: &'a str,
    operation_id: &'a str,
    step_id: &'a str,
    state_digest: &'a ContentDigest,
    state_generation: u64,
    runtime_inputs: &'a ContentDigest,
    policy_digest: &'a ContentDigest,
    provider_profile_digest: &'a ContentDigest,
    measurement_fingerprints: &'a [ContentDigest],
    selections: &'a [ContextSelection],
    input_budget_tokens: u32,
    projected_tokens: u32,
    pressure_ppm: u32,
    cache_prefix: Option<&'a CachePrefixBoundary>,
}

impl ContextPlan {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_id: impl Into<String>,
        step_id: impl Into<String>,
        state: &ContextState,
        policy_digest: ContentDigest,
        provider_profile_digest: ContentDigest,
        measurement_fingerprints: Vec<ContentDigest>,
        selections: Vec<ContextSelection>,
        input_budget_tokens: u32,
        projected_tokens: u32,
        pressure_ppm: u32,
        cache_prefix: Option<CachePrefixBoundary>,
        runtime_inputs: ContentDigest,
    ) -> Result<Self, ContextContractError> {
        let unsigned = Self {
            schema: CONTEXT_SCHEMA.to_string(),
            plan_id: ContentDigest::from_bytes(b"context-plan-placeholder"),
            operation_id: operation_id.into(),
            step_id: step_id.into(),
            state_digest: state.digest.clone(),
            state_generation: state.generation,
            runtime_inputs,
            policy_digest,
            provider_profile_digest,
            measurement_fingerprints,
            selections,
            input_budget_tokens,
            projected_tokens,
            pressure_ppm,
            cache_prefix,
        };
        let plan_id = digest(&ContextPlanBody::from(&unsigned))?;
        let plan = Self {
            plan_id,
            ..unsigned
        };
        plan.verify(state)?;
        Ok(plan)
    }

    pub fn verify(&self, state: &ContextState) -> Result<(), ContextContractError> {
        state.verify_digest()?;
        if self.state_generation != state.generation {
            return Err(ContextContractError::GenerationMismatch {
                expected: state.generation,
                actual: self.state_generation,
            });
        }
        if self.state_digest != state.digest {
            return Err(ContextContractError::DigestMismatch {
                kind: "plan state",
                expected: state.digest.clone(),
                actual: self.state_digest.clone(),
            });
        }
        let state_entries: HashSet<&str> = state
            .system
            .iter()
            .chain(&state.knowledge)
            .chain(&state.history)
            .chain(&state.state)
            .map(|entry| entry.entry_id.as_str())
            .collect();
        for selection in &self.selections {
            if !state_entries.contains(selection.entry_id.as_str()) {
                return Err(ContextContractError::UnknownEntry(
                    selection.entry_id.clone(),
                ));
            }
        }
        self.verify_digest()
    }

    pub fn verify_digest(&self) -> Result<(), ContextContractError> {
        verify_schema(&self.schema)?;
        if self.operation_id.is_empty() || self.step_id.is_empty() || self.pressure_ppm > 1_000_000
        {
            return Err(ContextContractError::FieldMismatch {
                field: "plan identity or pressure",
            });
        }
        let mut ids = HashSet::new();
        for selection in &self.selections {
            if !ids.insert(&selection.entry_id) {
                return Err(ContextContractError::DuplicateEntry(
                    selection.entry_id.clone(),
                ));
            }
        }
        let expected = digest(&ContextPlanBody::from(self))?;
        if expected != self.plan_id {
            return Err(ContextContractError::DigestMismatch {
                kind: "plan",
                expected,
                actual: self.plan_id.clone(),
            });
        }
        Ok(())
    }
}

impl<'a> From<&'a ContextPlan> for ContextPlanBody<'a> {
    fn from(value: &'a ContextPlan) -> Self {
        Self {
            schema: &value.schema,
            operation_id: &value.operation_id,
            step_id: &value.step_id,
            state_digest: &value.state_digest,
            state_generation: value.state_generation,
            runtime_inputs: &value.runtime_inputs,
            policy_digest: &value.policy_digest,
            provider_profile_digest: &value.provider_profile_digest,
            measurement_fingerprints: &value.measurement_fingerprints,
            selections: &value.selections,
            input_budget_tokens: value.input_budget_tokens,
            projected_tokens: value.projected_tokens,
            pressure_ppm: value.pressure_ppm,
            cache_prefix: value.cache_prefix.as_ref(),
        }
    }
}

/// The frozen identity of one provider execution input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextExecutionInput {
    pub schema: String,
    pub input_digest: ContentDigest,
    pub operation_id: String,
    pub step_id: String,
    pub input_sequence: u64,
    pub state_digest: ContentDigest,
    pub policy_digest: ContentDigest,
    pub plan_digest: ContentDigest,
    pub rendered_snapshot: ContentDigest,
    pub prompt_measurement: ContentDigest,
    pub provider_route: ContentDigest,
    pub cache_prefix: Option<CachePrefixBoundary>,
}

#[derive(Debug, Serialize)]
struct ContextExecutionInputBody<'a> {
    schema: &'a str,
    operation_id: &'a str,
    step_id: &'a str,
    input_sequence: u64,
    state_digest: &'a ContentDigest,
    policy_digest: &'a ContentDigest,
    plan_digest: &'a ContentDigest,
    rendered_snapshot: &'a ContentDigest,
    prompt_measurement: &'a ContentDigest,
    provider_route: &'a ContentDigest,
    cache_prefix: Option<&'a CachePrefixBoundary>,
}

impl ContextExecutionInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operation_id: impl Into<String>,
        step_id: impl Into<String>,
        input_sequence: u64,
        state: &ContextState,
        plan: &ContextPlan,
        rendered_snapshot: ContentDigest,
        prompt_measurement: ContentDigest,
        provider_route: ContentDigest,
    ) -> Result<Self, ContextContractError> {
        plan.verify(state)?;
        let operation_id = operation_id.into();
        let step_id = step_id.into();
        if operation_id != plan.operation_id {
            return Err(ContextContractError::FieldMismatch {
                field: "operation_id",
            });
        }
        if step_id != plan.step_id {
            return Err(ContextContractError::FieldMismatch { field: "step_id" });
        }
        let unsigned = Self {
            schema: CONTEXT_SCHEMA.to_string(),
            input_digest: ContentDigest::from_bytes(b"context-input-placeholder"),
            operation_id,
            step_id,
            input_sequence,
            state_digest: state.digest.clone(),
            policy_digest: plan.policy_digest.clone(),
            plan_digest: plan.plan_id.clone(),
            rendered_snapshot,
            prompt_measurement,
            provider_route,
            cache_prefix: plan.cache_prefix.clone(),
        };
        let input_digest = digest(&ContextExecutionInputBody::from(&unsigned))?;
        let input = Self {
            input_digest,
            ..unsigned
        };
        input.verify(plan)?;
        Ok(input)
    }

    pub fn verify(&self, plan: &ContextPlan) -> Result<(), ContextContractError> {
        verify_schema(&self.schema)?;
        plan.verify_digest()?;
        if self.provider_route != plan.provider_profile_digest {
            return Err(ContextContractError::FieldMismatch {
                field: "provider_route",
            });
        }
        if !plan
            .measurement_fingerprints
            .contains(&self.prompt_measurement)
        {
            return Err(ContextContractError::FieldMismatch {
                field: "prompt_measurement",
            });
        }
        if self.operation_id != plan.operation_id {
            return Err(ContextContractError::FieldMismatch {
                field: "operation_id",
            });
        }
        if self.step_id != plan.step_id {
            return Err(ContextContractError::FieldMismatch { field: "step_id" });
        }
        if self.state_digest != plan.state_digest {
            return Err(ContextContractError::FieldMismatch {
                field: "state_digest",
            });
        }
        if self.policy_digest != plan.policy_digest {
            return Err(ContextContractError::FieldMismatch {
                field: "policy_digest",
            });
        }
        if self.cache_prefix != plan.cache_prefix {
            return Err(ContextContractError::FieldMismatch {
                field: "cache_prefix",
            });
        }
        if self.plan_digest != plan.plan_id {
            return Err(ContextContractError::DigestMismatch {
                kind: "input plan",
                expected: plan.plan_id.clone(),
                actual: self.plan_digest.clone(),
            });
        }
        let expected = digest(&ContextExecutionInputBody::from(self))?;
        if expected != self.input_digest {
            return Err(ContextContractError::DigestMismatch {
                kind: "execution input",
                expected,
                actual: self.input_digest.clone(),
            });
        }
        Ok(())
    }
}

impl<'a> From<&'a ContextExecutionInput> for ContextExecutionInputBody<'a> {
    fn from(value: &'a ContextExecutionInput) -> Self {
        Self {
            schema: &value.schema,
            operation_id: &value.operation_id,
            step_id: &value.step_id,
            input_sequence: value.input_sequence,
            state_digest: &value.state_digest,
            policy_digest: &value.policy_digest,
            plan_digest: &value.plan_digest,
            rendered_snapshot: &value.rendered_snapshot,
            prompt_measurement: &value.prompt_measurement,
            provider_route: &value.provider_route,
            cache_prefix: value.cache_prefix.as_ref(),
        }
    }
}

/// The non-authoritative result returned by the preparation boundary. The provider adapter keeps
/// the projection transient and records the execution-input identity alongside host evidence.
#[derive(Debug, Clone)]
pub struct ContextPreparation {
    pub execution_input: ContextExecutionInput,
    pub plan: ContextPlan,
    pub rendered_projection: InternalRenderedContext,
}

/// Facts supplied by the runtime at the preparation boundary. The renderer remains a kernel
/// projection; provider adapters add their protocol-specific evidence after this call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPreparationRequest {
    pub operation_id: String,
    pub step_id: String,
    pub input_sequence: u64,
    pub policy_digest: ContentDigest,
    pub prompt_measurement: ContentDigest,
    pub provider_route: ContentDigest,
}

/// Kernel-owned facts frozen in a provider effect. Host route and native count do not exist
/// when the kernel emits this candidate; the host binds them before dispatch through core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCandidate {
    pub schema: String,
    pub operation_id: String,
    pub step_id: String,
    pub input_sequence: u64,
    pub state: ContextState,
    pub runtime_inputs: ContentDigest,
    pub policy_digest: ContentDigest,
    pub rendered_snapshot: ContentDigest,
    pub selections: Vec<ContextSelection>,
    pub input_budget_tokens: u32,
    pub projected_tokens: u32,
    pub pressure_ppm: u32,
    pub cache_prefix: Option<CachePrefixBoundary>,
}

impl ContextCandidate {
    pub fn bind(
        &self,
        prompt_measurement: ContentDigest,
        provider_route: ContentDigest,
    ) -> Result<(ContextPlan, ContextExecutionInput), ContextContractError> {
        verify_schema(&self.schema)?;
        let plan = ContextPlan::new(
            &self.operation_id,
            &self.step_id,
            &self.state,
            self.policy_digest.clone(),
            provider_route.clone(),
            vec![prompt_measurement.clone()],
            self.selections.clone(),
            self.input_budget_tokens,
            self.projected_tokens,
            self.pressure_ppm,
            self.cache_prefix.clone(),
            self.runtime_inputs.clone(),
        )?;
        let input = ContextExecutionInput::new(
            &self.operation_id,
            &self.step_id,
            self.input_sequence,
            &self.state,
            &plan,
            self.rendered_snapshot.clone(),
            prompt_measurement,
            provider_route,
        )?;
        Ok((plan, input))
    }
}

/// The host count names fingerprinted request material under the route's declared scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextPromptMeasurement {
    pub request_fingerprint: ContentDigest,
    pub input_tokens: u64,
    pub source: super::measurement::MeasurementSource,
    pub confidence: super::measurement::MeasurementConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextDispatchRequest {
    pub effect: crate::runtime::kernel::wire::effect::CallProviderEffect,
    pub request_fingerprint: ContentDigest,
    pub provider_route: serde_json::Value,
    pub prompt_measurement: ContextPromptMeasurement,
}

/// Evidence is returned with its canonical identity so storage adapters can persist every
/// referenced body. This is host evidence; the kernel's immutable candidate remains in the effect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextDispatchPreparation {
    pub execution_input: ContextExecutionInput,
    pub plan: ContextPlan,
    pub binding: crate::evolution::EvaluationContextBinding,
    pub state: ContextState,
    pub provider_route: serde_json::Value,
    pub prompt_measurement: ContextPromptMeasurement,
}

pub fn prepare_context_dispatch(
    request: &ContextDispatchRequest,
) -> Result<ContextDispatchPreparation, ContextContractError> {
    let candidate = &request.effect.context_candidate;
    let projection = digest(&(&request.effect.context, &request.effect.tools))?;
    if projection != candidate.rendered_snapshot {
        return Err(ContextContractError::DigestMismatch {
            kind: "provider projection",
            expected: candidate.rendered_snapshot.clone(),
            actual: projection,
        });
    }
    if request.prompt_measurement.request_fingerprint != request.request_fingerprint {
        return Err(ContextContractError::FieldMismatch {
            field: "measurement request fingerprint",
        });
    }
    let context = &request.effect.context;
    let prefix = match context.frozen_prefix_len {
        Some(entries) => {
            let turns = context.turns.get(..entries as usize).ok_or(
                ContextContractError::FieldMismatch {
                    field: "cache prefix boundary",
                },
            )?;
            Some(CachePrefixBoundary {
                entries,
                digest: digest(&(&context.system_stable, &context.system_knowledge, turns))?,
            })
        }
        None => None,
    };
    if prefix != candidate.cache_prefix {
        return Err(ContextContractError::FieldMismatch {
            field: "cache_prefix",
        });
    }
    if !request.provider_route.is_object()
        || request
            .provider_route
            .as_object()
            .is_some_and(|route| route.is_empty())
    {
        return Err(ContextContractError::FieldMismatch {
            field: "provider_route",
        });
    }
    let (plan, execution_input) = candidate.bind(
        digest(&request.prompt_measurement)?,
        digest(&request.provider_route)?,
    )?;
    let binding =
        crate::evolution::EvaluationContextBinding::from_execution_input(&execution_input);
    Ok(ContextDispatchPreparation {
        execution_input,
        plan,
        binding,
        state: candidate.state.clone(),
        provider_route: request.provider_route.clone(),
        prompt_measurement: request.prompt_measurement.clone(),
    })
}

/// Single canonical bridge shared by all SDK mirrors; no SDK owns selection or digest rules.
pub fn prepare_context_dispatch_json(request: &str) -> Result<String, String> {
    let request: ContextDispatchRequest =
        serde_json::from_str(request).map_err(|e| e.to_string())?;
    let preparation = prepare_context_dispatch(&request).map_err(|e| e.to_string())?;
    serde_json::to_string(&preparation).map_err(|e| e.to_string())
}

/// Rebind recorded host evidence to the replayed kernel effect and compare all derived objects.
/// A self-consistent record from a different effect, route or request is not equivalent.
pub fn verify_context_dispatch(
    effect: &crate::runtime::kernel::wire::effect::CallProviderEffect,
    preparation: &ContextDispatchPreparation,
) -> Result<(), ContextContractError> {
    let expected = prepare_context_dispatch(&ContextDispatchRequest {
        effect: effect.clone(),
        request_fingerprint: preparation.prompt_measurement.request_fingerprint.clone(),
        provider_route: preparation.provider_route.clone(),
        prompt_measurement: preparation.prompt_measurement.clone(),
    })?;
    let expected_digest = digest(&expected)?;
    let actual = digest(preparation)?;
    if expected_digest != actual {
        return Err(ContextContractError::DigestMismatch {
            kind: "replayed context execution",
            expected: expected_digest,
            actual,
        });
    }
    Ok(())
}

pub fn verify_context_dispatch_json(request: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        effect: crate::runtime::kernel::wire::effect::CallProviderEffect,
        preparation: ContextDispatchPreparation,
    }
    let request: Request = serde_json::from_str(request).map_err(|e| e.to_string())?;
    verify_context_dispatch(&request.effect, &request.preparation).map_err(|e| e.to_string())?;
    Ok("true".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::types::message::CoreMessage;

    fn digest(text: &str) -> ContentDigest {
        ContentDigest::from_bytes(text.as_bytes())
    }

    fn state() -> ContextState {
        let mut partitions = ContextPartitions::new(&ContextConfig::default());
        partitions.system.push(CoreMessage::system("rules"), 1);
        partitions.history.push(CoreMessage::user("hello"), 1);
        ContextState::from_partitions(&partitions, 3).unwrap()
    }

    fn dispatch_request() -> ContextDispatchRequest {
        use crate::runtime::kernel::wire::effect::{
            CallProviderEffect, RenderedContext as WireRenderedContext,
        };
        let manager = crate::context::manager::ContextManager::new(100_000);
        let (mut candidate, _) = manager
            .prepare_candidate("op".to_string(), "step".to_string(), 1, digest("policy"))
            .unwrap();
        let context = WireRenderedContext::default();
        let tools = vec![];
        candidate.rendered_snapshot = super::digest(&(&context, &tools)).unwrap();
        ContextDispatchRequest {
            effect: CallProviderEffect {
                context,
                tools,
                context_candidate: Box::new(candidate),
            },
            request_fingerprint: digest("actual request"),
            provider_route: serde_json::json!({"protocol":"test", "model":"model"}),
            prompt_measurement: ContextPromptMeasurement {
                request_fingerprint: digest("actual request"),
                input_tokens: 10,
                source: super::super::measurement::MeasurementSource::Heuristic,
                confidence: super::super::measurement::MeasurementConfidence::LowConfidence,
            },
        }
    }

    #[test]
    fn dispatch_replay_recomputes_the_same_input_and_rejects_changed_evidence() {
        let request = dispatch_request();
        let prepared = prepare_context_dispatch(&request).unwrap();
        verify_context_dispatch(&request.effect, &prepared).unwrap();
        let restored: ContextDispatchPreparation =
            serde_json::from_slice(&serde_json::to_vec(&prepared).unwrap()).unwrap();
        verify_context_dispatch(&request.effect, &restored).unwrap();
        let mut changed = restored.clone();
        changed.provider_route["model"] = "another-model".into();
        assert!(verify_context_dispatch(&request.effect, &changed).is_err());
        let mut changed = restored.clone();
        changed.prompt_measurement.input_tokens += 1;
        assert!(verify_context_dispatch(&request.effect, &changed).is_err());
        let mut changed = restored;
        changed.plan.selections[0].reason = "changed".to_string();
        assert!(changed.execution_input.verify(&changed.plan).is_err());
    }

    #[test]
    fn dispatch_rejects_projection_measurement_and_cache_mismatch() {
        let request = dispatch_request();
        let mut changed = request.clone();
        changed.effect.context.system_stable = "altered prompt".into();
        assert!(prepare_context_dispatch(&changed).is_err());
        let mut changed = request.clone();
        changed.prompt_measurement.request_fingerprint = digest("another request");
        assert!(prepare_context_dispatch(&changed).is_err());
        let mut changed = request;
        changed.effect.context_candidate.cache_prefix = Some(CachePrefixBoundary {
            digest: digest("forged cache"),
            entries: 99,
        });
        assert!(prepare_context_dispatch(&changed).is_err());
    }

    #[test]
    fn identical_unkeyed_knowledge_entries_have_distinct_identity() {
        let mut partitions = ContextPartitions::new(&ContextConfig::default());
        partitions.knowledge.push(CoreMessage::system("same"), 1);
        partitions.knowledge.push(CoreMessage::system("same"), 1);
        let state = ContextState::from_partitions(&partitions, 0).unwrap();
        assert_ne!(state.knowledge[0].entry_id, state.knowledge[1].entry_id);
        state.verify_digest().unwrap();
    }

    #[test]
    fn context_execution_shared_sdk_fixture() {
        let request = dispatch_request();
        let prepared = prepare_context_dispatch(&request).unwrap();
        verify_context_dispatch(&request.effect, &prepared).unwrap();
        let produced = serde_json::json!({
            "id": "context-execution", "domain": "context_execution",
            "input": { "request": request },
            "expected": { "canonical": {
                "input_digest": prepared.execution_input.input_digest,
                "plan_digest": prepared.plan.plan_id, "verified": true,
            } },
        });
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sdk-conformance/canonical/context-execution.json");
        if std::env::var("BLESS_CONTEXT_FIXTURES").as_deref() == Ok("1") {
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string_pretty(&produced).unwrap()),
            )
            .unwrap();
        }
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(produced, fixture, "shared SDK Context fixture drifted");
    }

    #[test]
    fn state_digest_covers_semantic_entries() {
        let state = state();
        state.verify_digest().unwrap();
        let mut changed = state.clone();
        changed.history[0].content_digest = digest("changed");
        assert!(changed.verify_digest().is_err());
    }

    #[test]
    fn plan_rejects_unknown_entries_and_tampering() {
        let state = state();
        let unknown = ContextSelection {
            entry_id: "history:missing".to_string(),
            action: ContextPlanAction::Include,
            reason: "test".to_string(),
        };
        assert!(
            ContextPlan::new(
                "op",
                "step",
                &state,
                digest("policy"),
                digest("route"),
                vec![],
                vec![unknown],
                100,
                10,
                50_000,
                None,
                digest("runtime-inputs"),
            )
            .is_err()
        );

        let selection = ContextSelection {
            entry_id: state.history[0].entry_id.clone(),
            action: ContextPlanAction::Include,
            reason: "fits".to_string(),
        };
        let mut plan = ContextPlan::new(
            "op",
            "step",
            &state,
            digest("policy"),
            digest("route"),
            vec![digest("measurement")],
            vec![selection],
            100,
            10,
            50_000,
            None,
            digest("runtime-inputs"),
        )
        .unwrap();
        plan.projected_tokens = 11;
        assert!(plan.verify(&state).is_err());
    }

    #[test]
    fn large_plan_validates_all_partitions_and_rejects_a_missing_tail_entry() {
        let mut partitions = ContextPartitions::new(&ContextConfig::default());
        partitions.system.push(CoreMessage::system("rules"), 1);
        partitions
            .knowledge
            .push(CoreMessage::system("reference"), 1);
        partitions.signals.push("current signal".to_string());
        for ordinal in 0..4096 {
            partitions
                .history
                .push(CoreMessage::user(format!("turn {ordinal}")), 1);
        }
        let state = ContextState::from_partitions(&partitions, 7).unwrap();
        let selections = state
            .system
            .iter()
            .chain(&state.knowledge)
            .chain(&state.history)
            .chain(&state.state)
            .rev()
            .map(|entry| ContextSelection {
                entry_id: entry.entry_id.clone(),
                action: ContextPlanAction::Include,
                reason: "selected".to_string(),
            })
            .collect::<Vec<_>>();
        let plan = ContextPlan::new(
            "op",
            "step",
            &state,
            digest("policy"),
            digest("route"),
            vec![digest("measurement")],
            selections.clone(),
            100_000,
            5000,
            50_000,
            None,
            digest("runtime"),
        )
        .unwrap();
        plan.verify(&state).unwrap();
        let mut invalid = selections;
        invalid.last_mut().unwrap().entry_id = "absent-entry".to_string();
        let rejected = ContextPlan::new(
            "op",
            "step",
            &state,
            digest("policy"),
            digest("route"),
            vec![digest("measurement")],
            invalid,
            100_000,
            5000,
            50_000,
            None,
            digest("runtime"),
        )
        .unwrap_err();
        assert_eq!(
            rejected,
            ContextContractError::UnknownEntry("absent-entry".to_string())
        );
    }

    #[test]
    fn execution_input_is_bound_to_plan_and_rejects_tampering() {
        let state = state();
        let selection = ContextSelection {
            entry_id: state.history[0].entry_id.clone(),
            action: ContextPlanAction::Include,
            reason: "fits".to_string(),
        };
        let plan = ContextPlan::new(
            "op",
            "step",
            &state,
            digest("policy"),
            digest("route"),
            vec![digest("measurement")],
            vec![selection],
            100,
            10,
            50_000,
            None,
            digest("runtime-inputs"),
        )
        .unwrap();
        let mut input = ContextExecutionInput::new(
            "op",
            "step",
            1,
            &state,
            &plan,
            digest("render"),
            digest("measurement"),
            digest("route"),
        )
        .unwrap();
        input.verify(&plan).unwrap();
        input.rendered_snapshot = digest("tampered-render");
        assert!(input.verify(&plan).is_err());
    }

    #[test]
    fn execution_input_cannot_relabel_a_plan() {
        let state = state();
        let plan = ContextPlan::new(
            "op",
            "step",
            &state,
            digest("policy"),
            digest("route"),
            vec![],
            vec![],
            100,
            10,
            50_000,
            None,
            digest("runtime-inputs"),
        )
        .unwrap();
        let error = ContextExecutionInput::new(
            "other-op",
            "step",
            1,
            &state,
            &plan,
            digest("render"),
            digest("measurement"),
            digest("route"),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ContextContractError::FieldMismatch {
                field: "operation_id"
            }
        );
    }
}
