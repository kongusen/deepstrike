//! Canonical host projection for the Rust production runner.
//!
//! Host events are lowered to wire inputs, committed through [`CanonicalKernelHost`], and projected
//! back to the [`HostAction`] / [`KernelObservation`] shapes the runner matches.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::runtime::CanonicalTransition;
use crate::runtime::canonical_kernel::{
    CanonicalKernel, EffectKind, EffectsDisposition, KernelEffect as WireKernelEffect,
    KernelTerminal, OperationLifecycle, PlannedStep, StepDisposition, TerminalDisposition,
    canonical_digest,
};
use crate::runtime::canonical_kernel_step::CanonicalKernelHost;
use crate::runtime::kernel_journal::KernelJournal;
use crate::{Error, Result};
use compact_str::CompactString;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
    MemoryTrustLevel,
};
use deepstrike_core::runtime::kernel::wire::CancellationReason;
use deepstrike_core::runtime::kernel::{KernelObservation, KernelPressureAction};
use deepstrike_core::types::message::{Content, Message, Role, ToolCall, ToolSchema};
use deepstrike_core::types::milestone::{MilestoneContract, MilestoneVerifier};
use deepstrike_core::types::result::{LoopResult, PaceAction, PaceDecision, TerminationReason};
use serde_json::{Map, Value, json};

use super::host_projection::{HostAction, HostEffect};

pub(crate) type PersistPayloadFn = Arc<
    dyn Fn(String, String, usize) -> Pin<Box<dyn Future<Output = Result<PersistedPayload>> + Send>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub(crate) struct PersistedPayload {
    pub payload_ref: String,
    pub digest: String,
    pub original_size: String,
    pub preview: String,
}

#[derive(Clone)]
pub(crate) struct CanonicalRunnerOptions {
    pub max_context_tokens: u32,
    pub max_turns: Option<u32>,
    pub max_total_tokens: Option<u64>,
    pub max_wall_ms: Option<u64>,
    pub memory_binding_id: String,
    pub persist_payload: Option<PersistPayloadFn>,
}

/// Canonical operation runtime for production runner callsites.
pub(crate) struct CanonicalRunnerRuntime {
    host: CanonicalKernelHost,
    config: Map<String, Value>,
    initial_context: InitialContext,
    memory_binding_id: String,
    persist_payload: Option<PersistPayloadFn>,
    configured: bool,
    started: bool,
    last_action: Option<HostAction>,
    observations: Vec<KernelObservation>,
    milestone_phases: std::collections::HashMap<String, MilestonePhaseProjection>,
    payload_inline_threshold: usize,
    payload_preview_bytes: usize,
}

#[derive(Debug, Clone, Default)]
struct InitialContext {
    messages: Vec<Value>,
    knowledge: Vec<Value>,
    capabilities: Vec<Value>,
}

#[derive(Debug, Clone, Default)]
struct MilestonePhaseProjection {
    criteria: Vec<String>,
    verifier: Option<MilestoneVerifier>,
    required_evidence: Vec<String>,
}

impl CanonicalRunnerRuntime {
    pub fn new(
        kernel: CanonicalKernel,
        journal: Arc<dyn KernelJournal>,
        operation_id: impl Into<String>,
        options: CanonicalRunnerOptions,
    ) -> Result<Self> {
        let mut execution_policy = Map::new();
        execution_policy.insert(
            "max_context_tokens".into(),
            json!(options.max_context_tokens),
        );
        if let Some(max_turns) = options.max_turns {
            execution_policy.insert("max_turns".into(), json!(max_turns));
        }
        if let Some(max_total_tokens) = options.max_total_tokens {
            execution_policy.insert(
                "max_total_tokens".into(),
                Value::String(max_total_tokens.to_string()),
            );
        }
        if let Some(max_wall_ms) = options.max_wall_ms {
            execution_policy.insert("max_wall_ms".into(), Value::String(max_wall_ms.to_string()));
        }

        let mut config = Map::new();
        config.insert("execution_policy".into(), Value::Object(execution_policy));
        config.insert(
            "host_effect_support".into(),
            json!({
                "supported": [
                    "call_provider", "execute_tools", "request_approval", "spawn_tasks",
                    "preempt_tasks", "persist_memory", "query_memory", "archive_page_out",
                    "load_payload", "evaluate_milestone",
                ]
            }),
        );
        config.insert(
            "kernel_limits".into(),
            json!({
                "max_json_depth": 64,
                "max_collection_entries": 65_536,
                "collection_limits": {
                    "tool_catalog": 4_096,
                    "skill_catalog": 4_096,
                    "knowledge_entries": 65_536,
                    "initial_messages": 65_536,
                    "capability_grants": 65_536,
                    "governance_rules": 65_536,
                },
            }),
        );

        Ok(Self {
            host: CanonicalKernelHost::new(kernel, journal, operation_id)?,
            config,
            initial_context: InitialContext::default(),
            memory_binding_id: options.memory_binding_id,
            persist_payload: options.persist_payload,
            configured: false,
            started: false,
            last_action: None,
            observations: Vec::new(),
            milestone_phases: std::collections::HashMap::new(),
            payload_inline_threshold: 50 * 1024,
            payload_preview_bytes: 2 * 1024,
        })
    }

    pub fn operation_id(&self) -> &str {
        self.host.operation_id()
    }

    pub fn turn(&self) -> u32 {
        self.host.turn()
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.host.lifecycle(),
            OperationLifecycle::Completed
                | OperationLifecycle::Cancelled
                | OperationLifecycle::Failed
        )
    }

    pub fn recovery_content_bytes(&self) -> usize {
        if let Some(bytes) = self.host.recovery_content_bytes() {
            return bytes;
        }
        let max = self
            .config
            .get("execution_policy")
            .and_then(|v| v.get("max_context_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        max.saturating_mul(4).max(1024)
    }

    pub fn preserved_refs(&self) -> Vec<String> {
        self.host.preserved_refs()
    }

    pub fn count_tokens(&self, text: &str) -> u32 {
        self.host
            .count_tokens(text)
            .unwrap_or_else(|| ((text.len() / 4) as u32).max(1))
    }

    #[cfg(test)]
    pub fn local_subagents_spawned(&self) -> usize {
        self.host.local_subagents_spawned()
    }

    pub fn drain_host_observations(&mut self) -> Vec<KernelObservation> {
        std::mem::take(&mut self.observations)
    }

    pub fn drain_new_messages(&mut self) -> Vec<Message> {
        self.host.new_messages()
    }

    #[cfg(test)]
    pub fn pending_effect_count(&self) -> usize {
        self.host.pending_effects().len()
    }

    pub fn remember_milestone_contract(&mut self, contract: &MilestoneContract) {
        self.milestone_phases.clear();
        for phase in &contract.phases {
            self.milestone_phases.insert(
                phase.id.clone(),
                MilestonePhaseProjection {
                    criteria: phase.criteria.clone(),
                    verifier: phase.verifier.clone(),
                    required_evidence: phase.required_evidence.clone(),
                },
            );
        }
    }

    pub async fn restore(&mut self) -> Result<()> {
        self.host.restore().await?;
        let lifecycle = self.host.lifecycle();
        self.configured = !matches!(lifecycle, OperationLifecycle::Created);
        self.started = !matches!(
            lifecycle,
            OperationLifecycle::Created | OperationLifecycle::Configured
        );
        if let Some(transition) = self.host.drain_outbound_envelope().await? {
            self.apply_transition(&transition, true)?;
        }
        self.last_action = self.current_action()?;
        Ok(())
    }

    pub fn resume_action(&mut self) -> Result<Option<HostAction>> {
        self.last_action = self.current_action()?;
        Ok(self.last_action.clone())
    }

    pub async fn start_agent_value(
        &mut self,
        task: Value,
        run_spec: Option<Value>,
    ) -> Result<Option<HostAction>> {
        self.ensure_configured().await?;
        let task = object(Some(&task));
        let goal = string_field(&task, "goal");
        let mut entry = json!({
            "kind": "agent",
            "task": {
                "goal": goal,
                "criteria": task.get("criteria").cloned().unwrap_or_else(|| json!([])),
            },
        });
        if let Some(run_spec) = run_spec {
            entry.as_object_mut().unwrap().insert(
                "run_spec".into(),
                logical_run_spec(object(Some(&run_spec)), &goal),
            );
        }
        let action = self
            .commit(json!({
                "kind": "start_operation",
                "entry": entry,
                "initial_context": self.initial_context_json(),
            }))
            .await?;
        self.started = true;
        Ok(action)
    }

    #[cfg(test)]
    pub async fn start_workflow_value(&mut self, spec: Value) -> Result<Option<HostAction>> {
        self.ensure_configured().await?;
        let action = self
            .commit(json!({
                "kind": "start_operation",
                "entry": {
                    "kind": "workflow",
                    "spec": self.workflow_spec(object(Some(&spec))),
                },
                "initial_context": self.initial_context_json(),
            }))
            .await?;
        self.started = true;
        Ok(action)
    }

    /// Lower one SDK-owned host fact into the canonical five-class input taxonomy.
    ///
    /// This is intentionally a JSON-shaped host boundary: the canonical wire DTOs remain owned by
    /// core, while the SDK no longer imports or constructs the retired input enum.
    pub async fn apply_host_event(&mut self, event: Value) -> Result<Option<HostAction>> {
        if !self.started && self.apply_bootstrap(&event) {
            return Ok(None);
        }
        let kind = event
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match kind {
            "provider_result" => {
                let message = object(event.get("message"));
                let mut outcome = json!({
                    "kind": "completed",
                    "message": provider_message(&message),
                });
                if let Some(v) = event.get("observed_input_tokens") {
                    outcome
                        .as_object_mut()
                        .unwrap()
                        .insert("observed_input_tokens".into(), v.clone());
                }
                if let Some(v) = event.get("observed_output_tokens") {
                    outcome
                        .as_object_mut()
                        .unwrap()
                        .insert("observed_output_tokens".into(), v.clone());
                }
                if let Some(reason) = provider_stop_reason(event.get("stop_reason")) {
                    outcome
                        .as_object_mut()
                        .unwrap()
                        .insert("stop_reason".into(), Value::String(reason));
                }
                self.resolve(&event, json!({ "kind": "provider", "outcome": outcome }))
                    .await
            }
            "provider_error" => {
                let message = string_value(&event, "message");
                if regex_context_overflow(&message) {
                    self.resolve(
                        &event,
                        json!({ "kind": "provider", "outcome": { "kind": "context_overflow" } }),
                    )
                    .await
                } else {
                    self.failed(&event, "transport_exhausted", &message, true)
                        .await
                }
            }
            "tool_results" => {
                let mut results = Vec::new();
                for raw in event
                    .get("results")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    let result = object(Some(&raw));
                    let call_id = string_field(&result, "call_id");
                    let output = string_field(&result, "output");
                    let is_error = result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    let disposition = if result
                        .get("is_fatal")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        "fatal"
                    } else {
                        "recoverable"
                    };
                    let bytes = output.len();
                    if bytes > self.payload_inline_threshold {
                        if let Some(persist) = &self.persist_payload {
                            let persisted =
                                persist(call_id.clone(), output.clone(), self.payload_preview_bytes)
                                    .await?;
                            let mut external = json!({
                                "kind": "external",
                                "call_id": call_id,
                                "payload_ref": persisted.payload_ref,
                                "digest": persisted.digest,
                                "original_size": persisted.original_size,
                                "preview": persisted.preview,
                                "disposition": disposition,
                            });
                            if is_error {
                                external
                                    .as_object_mut()
                                    .unwrap()
                                    .insert("is_error".into(), json!(true));
                            }
                            results.push(external);
                            continue;
                        }
                    }
                    let mut inline_result = json!({
                        "output": output,
                        "disposition": disposition,
                    });
                    if is_error {
                        inline_result
                            .as_object_mut()
                            .unwrap()
                            .insert("is_error".into(), json!(true));
                    }
                    if let Some(tokens) = result.get("token_count") {
                        inline_result
                            .as_object_mut()
                            .unwrap()
                            .insert("tokens".into(), tokens.clone());
                    }
                    results.push(json!({
                        "kind": "inline",
                        "call_id": call_id,
                        "result": inline_result,
                    }));
                }
                self.resolve(&event, json!({ "kind": "tools", "results": results }))
                    .await
            }
            "approval_result" => {
                self.resolve(
                    &event,
                    json!({
                        "kind": "approval",
                        "approved_call_ids": event.get("approved_calls").cloned().unwrap_or_else(|| json!([])),
                        "denied_call_ids": event.get("denied_calls").cloned().unwrap_or_else(|| json!([])),
                    }),
                )
                .await
            }
            "workflow_spawn_result" => {
                let attempts_by_id = self.spawn_attempts();
                let attempts: Vec<Value> = attempts_by_id
                    .iter()
                    .map(|(task_id, attempt_id)| {
                        json!({
                            "task_id": task_id,
                            "attempt_id": attempt_id,
                            "outcome": { "status": "started" },
                        })
                    })
                    .collect();
                self.resolve(
                    &event,
                    json!({ "kind": "tasks_spawned", "attempts": attempts }),
                )
                .await
            }
            "preempt_result" => {
                let attempts_by_id = self.preempt_attempts();
                let attempts: Vec<Value> = attempts_by_id
                    .iter()
                    .map(|(task_id, attempt_id)| {
                        json!({
                            "task_id": task_id,
                            "attempt_id": attempt_id,
                            "outcome": { "status": "preempted" },
                        })
                    })
                    .collect();
                self.resolve(
                    &event,
                    json!({ "kind": "tasks_preempted", "attempts": attempts }),
                )
                .await
            }
            "sub_agent_completed" => {
                let raw = object(event.get("result"));
                let result = object(raw.get("result"));
                let task_id = string_field(&raw, "agent_id");
                let attempt_id = self
                    .host
                    .attempt_id(&task_id)
                    .ok_or_else(|| {
                        Error::Other(format!(
                            "sub-agent completion names task {task_id:?} with no live kernel-issued attempt"
                        ))
                    })?;
                let final_message = object(result.get("final_message"));
                let termination = string_field(&result, "termination");
                let status = if termination == "completed" {
                    "completed"
                } else {
                    "failed"
                };
                let mut child_result = json!({
                    "status": status,
                    "usage": {
                        "input_tokens": "0",
                        "output_tokens": result.get("total_tokens_used").map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
                        "turns": result.get("turns_used").and_then(|v| v.as_u64()).unwrap_or(0),
                    },
                });
                if let Some(content) = final_message.get("content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        child_result
                            .as_object_mut()
                            .unwrap()
                            .insert("output".into(), Value::String(content.to_string()));
                    }
                }
                if !matches!(
                    termination.as_str(),
                    "completed" | "max_turns" | "token_budget" | ""
                ) {
                    child_result.as_object_mut().unwrap().insert(
                        "error".into(),
                        Value::String(if termination.is_empty() {
                            "failed".into()
                        } else {
                            termination
                        }),
                    );
                }
                let mut child = json!({
                    "kind": "child_completed",
                    "task_id": task_id,
                    "attempt_id": attempt_id,
                    "result": child_result,
                });
                let submitted = raw
                    .get("submitted_nodes")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if !submitted.is_empty() {
                    let nodes = self
                        .workflow_spec(object(Some(&json!({ "nodes": submitted }))))
                        .get("nodes")
                        .cloned()
                        .unwrap_or_else(|| json!([]));
                    child.as_object_mut().unwrap().insert(
                        "parent_requests".into(),
                        json!([{ "kind": "append_workflow_nodes", "nodes": nodes }]),
                    );
                }
                self.commit(json!({
                    "kind": "deliver_external_event",
                    "event": child,
                }))
                .await
            }
            "memory_persist_result" => {
                if let Some(error) = event.get("error").filter(|v| !v.is_null()) {
                    self.failed(&event, "storage_unavailable", &error.to_string(), true)
                        .await
                } else {
                    let record_ref = event
                        .get("record_ref")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("memory:{}", uuid::Uuid::new_v4()));
                    let digest = event
                        .get("digest")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            sha256_digest(
                                event
                                    .get("record_ref")
                                    .or_else(|| event.get("effect_id"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(""),
                            )
                        });
                    self.resolve(
                        &event,
                        json!({
                            "kind": "memory_persisted",
                            "receipt": {
                                "binding_id": self.memory_binding_id,
                                "record_ref": record_ref,
                                "digest": digest,
                            },
                        }),
                    )
                    .await
                }
            }
            "memory_query_result" => {
                if let Some(error) = event.get("error").filter(|v| !v.is_null()) {
                    self.failed(&event, "storage_unavailable", &error.to_string(), true)
                        .await
                } else {
                    let recalls: Vec<Value> = event
                        .get("hits")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|raw| {
                            let hit = object(Some(&raw));
                            let record = object(hit.get("record"));
                            let mut recall = json!({
                                "record_ref": record.get("record_id").and_then(|v| v.as_str()).unwrap_or(&format!("memory:{}", uuid::Uuid::new_v4())),
                                "name": string_field(&record, "name"),
                                "kind": record.get("kind").and_then(|v| v.as_str()).unwrap_or("reference"),
                                "content": string_field(&record, "content"),
                            });
                            if let Some(score) = hit.get("score").filter(|v| v.is_number()) {
                                recall
                                    .as_object_mut()
                                    .unwrap()
                                    .insert("score".into(), score.clone());
                            }
                            recall
                        })
                        .collect();
                    self.resolve(
                        &event,
                        json!({ "kind": "memory_queried", "recalls": recalls }),
                    )
                    .await
                }
            }
            "page_out_archive_result" => {
                if let Some(error) = event.get("error").filter(|v| !v.is_null()) {
                    self.failed(&event, "storage_unavailable", &error.to_string(), true)
                        .await
                } else {
                    let effect_id = string_value(&event, "effect_id");
                    let pending = self
                        .host
                        .pending_effects()
                        .into_iter()
                        .find(|effect| effect.effect_id.as_str() == effect_id);
                    let (handle_id, digest, original_size) = match pending.as_ref().map(|e| &e.effect)
                    {
                        Some(EffectKind::ArchivePageOut(archive)) => (
                            archive.handle_id.as_str().to_string(),
                            archive.payload.digest.as_str().to_string(),
                            archive.payload.original_size.get().to_string(),
                        ),
                        _ => (
                            String::new(),
                            sha256_digest(""),
                            "0".to_string(),
                        ),
                    };
                    let payload_ref = event
                        .get("archive_ref")
                        .or_else(|| event.get("payload_ref"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("payload:{}", uuid::Uuid::new_v4()));
                    self.resolve(
                        &event,
                        json!({
                            "kind": "page_out_archived",
                            "receipt": {
                                "handle_id": handle_id,
                                "payload_ref": payload_ref,
                                "digest": digest,
                                "original_size": original_size,
                            },
                        }),
                    )
                    .await
                }
            }
            "milestone_result" => {
                let result = object(event.get("result"));
                let mut body = json!({
                    "phase_id": string_field(&result, "phase_id"),
                    "passed": result.get("passed").and_then(|v| v.as_bool()).unwrap_or(false),
                });
                if !body["passed"].as_bool().unwrap_or(false) {
                    if let Some(reason) = result.get("reason").and_then(|v| v.as_str()) {
                        body.as_object_mut()
                            .unwrap()
                            .insert("notes".into(), Value::String(reason.to_string()));
                    }
                }
                self.resolve(
                    &event,
                    json!({ "kind": "milestone_evaluated", "result": body }),
                )
                .await
            }
            "payload_loaded" => {
                let payload = event.get("payload").cloned().unwrap_or_else(|| {
                    json!({
                        "content": string_value(&event, "content"),
                        "digest": string_value(&event, "digest"),
                        "original_size": event
                            .get("original_size")
                            .map(|value| match value {
                                Value::String(value) => value.clone(),
                                other => other.to_string(),
                            })
                            .unwrap_or_else(|| "0".into()),
                    })
                });
                self.resolve(
                    &event,
                    json!({
                        "kind": "payload_loaded",
                        "handle_id": string_value(&event, "handle_id"),
                        "payload": payload,
                    }),
                )
                .await
            }
            "payload_load_failed" => {
                self.failed(
                    &event,
                    "storage_unavailable",
                    &string_value(&event, "error"),
                    true,
                )
                .await
            }
            "deliver_signal" => {
                self.commit(json!({
                    "kind": "deliver_external_event",
                    "event": canonical_signal(&event),
                }))
                .await
            }
            "add_knowledge_message" => {
                if !self.started {
                    return Ok(None);
                }
                let mut entry = json!({ "content": string_value(&event, "content") });
                if let Some(key) = event.get("key") {
                    entry.as_object_mut().unwrap().insert("key".into(), key.clone());
                }
                if let Some(tokens) = event.get("tokens") {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("tokens".into(), tokens.clone());
                }
                if event.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false) {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("pinned".into(), json!(true));
                }
                self.commit(json!({
                    "kind": "host_control",
                    "command": { "kind": "seed_knowledge", "entries": [entry] },
                }))
                .await
            }
            "remove_knowledge" => {
                self.commit(json!({
                    "kind": "host_control",
                    "command": {
                        "kind": "apply_knowledge_mutation",
                        "mutation": { "remove": [string_value(&event, "key")] },
                    },
                }))
                .await
            }
            "skill_deactivated" => {
                self.commit(json!({
                    "kind": "host_control",
                    "command": {
                        "kind": "apply_skill_activation",
                        "deactivate": [string_value(&event, "name")],
                    },
                }))
                .await
            }
            "skill_activated" => Ok(self.last_action.clone()),
            "capability_command" => {
                self.commit(json!({
                    "kind": "host_control",
                    "command": canonical_capability_command(object(event.get("command"))),
                }))
                .await
            }
            "add_history_message" => Err(Error::Other(
                "unsupported_host_event: running ABI v3 operations accept history only through effects or external events".into(),
            )),
            "cancel_operation" => {
                let reason = event
                    .get("reason")
                    .cloned()
                    .unwrap_or_else(|| json!("user"));
                let pending_call_ids = event
                    .get("pending_call_ids")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let action = self
                    .commit(json!({
                        "kind": "host_control",
                        "command": {
                            "kind": "cancel",
                            "reason": reason,
                            "pending_call_ids": pending_call_ids,
                        },
                    }))
                    .await?;
                self.observations
                    .push(KernelObservation::OperationCancelled {
                        turn: self.turn(),
                        operation_id: self.operation_id().to_string(),
                        reason: CancellationReason::User,
                        pending_call_ids: event
                            .get("pending_call_ids")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    });
                Ok(action)
            }
            "update_task" => {
                self.commit(json!({
                    "kind": "host_control",
                    "command": {
                        "kind": "update_task",
                        "update": event.get("update").cloned().unwrap_or(Value::Null),
                    },
                }))
                .await
            }
            other => Err(Error::Other(format!(
                "unsupported_host_event: canonical ABI has no lowering for {other}"
            ))),
        }
    }

    async fn ensure_configured(&mut self) -> Result<()> {
        if !self.configured {
            self.commit(json!({
                "kind": "configure_operation",
                "config": Value::Object(self.config.clone()),
            }))
            .await?;
            self.configured = true;
        }
        Ok(())
    }

    async fn commit(&mut self, input: Value) -> Result<Option<HostAction>> {
        let transition = self.host.transition_input(input).await?;
        let publish_observations = !transition.replayed;
        self.apply_transition(&transition, publish_observations)
    }

    fn apply_transition(
        &mut self,
        transition: &CanonicalTransition,
        publish_observations: bool,
    ) -> Result<Option<HostAction>> {
        if publish_observations {
            self.observations
                .extend(transition.planned_step.observations.clone());
        }
        self.last_action = self.enrich_action(canonical_action_from_planned_step(
            &transition.planned_step,
        )?);
        Ok(self.last_action.clone())
    }

    async fn resolve(&mut self, event: &Value, result: Value) -> Result<Option<HostAction>> {
        self.commit(json!({
            "kind": "resolve_effect",
            "effect_id": string_value(event, "effect_id"),
            "outcome": { "status": "succeeded", "result": result },
        }))
        .await
    }

    async fn failed(
        &mut self,
        event: &Value,
        kind: &str,
        message: &str,
        retryable: bool,
    ) -> Result<Option<HostAction>> {
        self.commit(json!({
            "kind": "resolve_effect",
            "effect_id": string_value(event, "effect_id"),
            "outcome": {
                "status": "failed",
                "failure": {
                    "kind": kind,
                    "message": message,
                    "retryable": retryable,
                },
            },
        }))
        .await
    }

    fn current_action(&self) -> Result<Option<HostAction>> {
        if let Some(terminal) = self.host.terminal() {
            return canonical_action_from_planned_step(&PlannedStep {
                root_kind: None,
                focus: None,
                observations: Vec::new(),
                disposition: StepDisposition::Terminal(TerminalDisposition {
                    terminal: terminal.clone(),
                }),
            });
        }
        let effects = self.host.pending_effects();
        Ok(
            self.enrich_action(canonical_action_from_planned_step(&PlannedStep {
                root_kind: None,
                focus: None,
                observations: Vec::new(),
                disposition: StepDisposition::Effects(EffectsDisposition { effects }),
            })?),
        )
    }

    fn enrich_action(&self, mut action: Option<HostAction>) -> Option<HostAction> {
        if let Some(HostAction {
            effect:
                HostEffect::EvaluateMilestone {
                    phase_id,
                    criteria,
                    verifier,
                    required_evidence,
                },
            ..
        }) = action.as_mut()
            && let Some(phase) = self.milestone_phases.get(phase_id)
        {
            *criteria = phase.criteria.clone();
            *verifier = phase.verifier.clone();
            *required_evidence = phase.required_evidence.clone();
        }
        action
    }

    fn spawn_attempts(&self) -> Vec<(String, String)> {
        let mut attempts = Vec::new();
        for effect in self.host.pending_effects() {
            if let EffectKind::SpawnTasks(spawn) = &effect.effect {
                for task in &spawn.tasks {
                    attempts.push((
                        task.task_id.as_str().to_string(),
                        task.attempt_id.as_str().to_string(),
                    ));
                }
            }
        }
        attempts
    }

    fn preempt_attempts(&self) -> Vec<(String, String)> {
        let mut attempts = Vec::new();
        for effect in self.host.pending_effects() {
            if let EffectKind::PreemptTasks(preempt) = &effect.effect {
                for attempt in &preempt.attempts {
                    attempts.push((
                        attempt.task_id.as_str().to_string(),
                        attempt.attempt_id.as_str().to_string(),
                    ));
                }
            }
        }
        attempts
    }

    fn initial_context_json(&self) -> Value {
        json!({
            "messages": self.initial_context.messages,
            "knowledge": self.initial_context.knowledge,
            "capabilities": self.initial_context.capabilities,
        })
    }

    fn feature_policy(&mut self) -> &mut Map<String, Value> {
        if !self.config.contains_key("feature_policy") {
            self.config
                .insert("feature_policy".into(), Value::Object(Map::new()));
        }
        self.config
            .get_mut("feature_policy")
            .unwrap()
            .as_object_mut()
            .unwrap()
    }

    fn execution_policy(&mut self) -> &mut Map<String, Value> {
        if !self.config.contains_key("execution_policy") {
            self.config
                .insert("execution_policy".into(), Value::Object(Map::new()));
        }
        self.config
            .get_mut("execution_policy")
            .unwrap()
            .as_object_mut()
            .unwrap()
    }

    fn context_policy(&mut self) -> &mut Map<String, Value> {
        if !self.config.contains_key("context_policy") {
            self.config
                .insert("context_policy".into(), Value::Object(Map::new()));
        }
        self.config
            .get_mut("context_policy")
            .unwrap()
            .as_object_mut()
            .unwrap()
    }

    fn apply_bootstrap(&mut self, event: &Value) -> bool {
        let kind = event
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match kind {
            "set_tokenizer" => true,
            "set_tools" => {
                self.config.insert(
                    "tool_catalog".into(),
                    event.get("tools").cloned().unwrap_or_else(|| json!([])),
                );
                true
            }
            "add_system_message" => {
                let mut message = json!({
                    "role": "system",
                    "content": string_value(event, "content"),
                });
                if let Some(tokens) = event.get("tokens") {
                    message
                        .as_object_mut()
                        .unwrap()
                        .insert("tokens".into(), tokens.clone());
                }
                self.initial_context.messages.push(message);
                true
            }
            "add_knowledge_message" => {
                let mut entry = json!({ "content": string_value(event, "content") });
                if let Some(key) = event.get("key") {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("key".into(), key.clone());
                }
                if let Some(tokens) = event.get("tokens") {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("tokens".into(), tokens.clone());
                }
                if event
                    .get("pinned")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    entry
                        .as_object_mut()
                        .unwrap()
                        .insert("pinned".into(), json!(true));
                }
                self.initial_context.knowledge.push(entry);
                true
            }
            "preload_history" => {
                for message in event
                    .get("messages")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    self.initial_context
                        .messages
                        .push(initial_message(object(Some(&message))));
                }
                true
            }
            "set_available_skills" => {
                self.config.insert(
                    "skill_catalog".into(),
                    event.get("skills").cloned().unwrap_or_else(|| json!([])),
                );
                true
            }
            "load_governance_policy" => {
                let mut governance = if let Some(policy) = event.get("policy") {
                    object(Some(policy))
                } else {
                    let mut flat = object(Some(event));
                    flat.remove("kind");
                    flat
                };
                if let Some(rate_limits) = governance
                    .get_mut("rate_limits")
                    .and_then(|v| v.as_array_mut())
                {
                    for rule in rate_limits {
                        if let Some(obj) = rule.as_object_mut() {
                            if let Some(window) = obj.get("window_ms").cloned() {
                                obj.insert(
                                    "window_ms".into(),
                                    Value::String(match window {
                                        Value::String(s) => s,
                                        other => other.to_string(),
                                    }),
                                );
                            }
                        }
                    }
                }
                self.config
                    .insert("governance_policy".into(), Value::Object(governance));
                true
            }
            "set_signal_policy" => {
                let mut signal = object(event.get("policy"));
                signal.remove("version");
                if let Some(ttl) = signal.get("ttl_ms").cloned() {
                    signal.insert(
                        "ttl_ms".into(),
                        Value::String(match ttl {
                            Value::String(s) => s,
                            other => other.to_string(),
                        }),
                    );
                }
                self.config
                    .insert("signal_policy".into(), Value::Object(signal));
                true
            }
            "set_plan_tool_enabled" => {
                self.feature_policy().insert(
                    "plan_tool_enabled".into(),
                    json!(
                        event
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    ),
                );
                true
            }
            "set_stable_core_tools" => {
                self.feature_policy().insert(
                    "stable_core_tool_ids".into(),
                    event.get("tool_ids").cloned().unwrap_or_else(|| json!([])),
                );
                true
            }
            "set_memory_enabled" => {
                let enabled = event
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.feature_policy()
                    .insert("memory_enabled".into(), json!(enabled));
                if enabled {
                    self.config.insert(
                        "memory_access".into(),
                        json!({
                            "binding_id": self.memory_binding_id,
                            "capabilities": { "read": true, "write": true },
                        }),
                    );
                }
                true
            }
            "set_knowledge_enabled" => {
                self.feature_policy().insert(
                    "knowledge_enabled".into(),
                    json!(
                        event
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    ),
                );
                true
            }
            "set_resource_quota" => {
                let mut quota = object(event.get("quota"));
                if let Some(window) = quota
                    .get("memory_writes_per_window")
                    .and_then(|v| v.as_array())
                    .cloned()
                {
                    quota.insert(
                        "memory_writes_per_window".into(),
                        json!({
                            "max_events": window.first().and_then(|v| v.as_u64()).unwrap_or(0),
                            "window_ms": window.get(1).map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
                        }),
                    );
                }
                self.config
                    .insert("resource_quota".into(), Value::Object(quota));
                true
            }
            "set_repeat_fuse" => {
                let mut fuse = object(Some(event));
                fuse.remove("kind");
                self.execution_policy()
                    .insert("repeat_fuse".into(), Value::Object(fuse));
                true
            }
            "set_criteria_gate" => {
                self.execution_policy().insert(
                    "criteria_gate_enabled".into(),
                    json!(
                        event
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true)
                    ),
                );
                true
            }
            "set_knowledge_budget" => {
                let ratio = event.get("ratio").and_then(|v| v.as_f64()).unwrap_or(0.0);
                self.context_policy().insert(
                    "knowledge_budget_ppm".into(),
                    json!((ratio * 1_000_000.0).round() as u64),
                );
                true
            }
            "set_entropy_watch" => {
                let mut watch = object(Some(event));
                watch.remove("kind");
                if let Some(threshold) = watch.remove("threshold").and_then(|v| v.as_f64()) {
                    watch.insert(
                        "threshold_ppm".into(),
                        json!((threshold * 1_000_000.0).round() as u64),
                    );
                }
                if let Some(hysteresis) = watch.remove("hysteresis").and_then(|v| v.as_f64()) {
                    watch.insert(
                        "hysteresis_ppm".into(),
                        json!((hysteresis * 1_000_000.0).round() as u64),
                    );
                }
                self.execution_policy()
                    .insert("entropy_watch".into(), Value::Object(watch));
                true
            }
            "set_memory_policy" => {
                let mut policy = Map::new();
                for key in [
                    "stale_warning_days",
                    "retrieval_top_k",
                    "validation_enabled",
                    "max_content_bytes",
                    "max_name_length",
                ] {
                    if let Some(value) = event.get(key) {
                        policy.insert(key.into(), value.clone());
                    }
                }
                if let Some(value) = event.get("promotion_recall_threshold") {
                    policy.insert(
                        "promotion_recall_threshold".into(),
                        Value::String(match value {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        }),
                    );
                }
                self.config
                    .insert("memory_policy".into(), Value::Object(policy));
                true
            }
            "load_milestone_contract" => {
                let contract = object(event.get("contract"));
                self.milestone_phases.clear();
                let phases: Vec<Value> = contract
                    .get("phases")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|phase| {
                        let phase = object(Some(&phase));
                        let phase_id = phase
                            .get("id")
                            .or_else(|| phase.get("phase_id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let criteria = string_array(phase.get("criteria"));
                        let required_evidence = string_array(phase.get("required_evidence"));
                        let verifier = phase
                            .get("verifier")
                            .filter(|value| !value.is_null())
                            .cloned()
                            .and_then(|value| serde_json::from_value(value).ok());
                        self.milestone_phases.insert(
                            phase_id.clone(),
                            MilestonePhaseProjection {
                                criteria,
                                verifier,
                                required_evidence,
                            },
                        );
                        let unlocks: Vec<Value> = phase
                            .get("unlocks")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|item| {
                                if let Some(s) = item.as_str() {
                                    json!(s)
                                } else {
                                    json!(string_field(&object(Some(&item)), "id"))
                                }
                            })
                            .collect();
                        json!({
                            "phase_id": phase_id,
                            "unlocks": unlocks,
                        })
                    })
                    .collect();
                self.config.insert(
                    "verification_contracts".into(),
                    json!([{ "contract_id": "rust-default", "phases": phases }]),
                );
                true
            }
            "add_history_message" => {
                self.initial_context
                    .messages
                    .push(initial_message(object(event.get("message"))));
                true
            }
            "configure_run" => {
                self.merge_host_config(object(event.get("config")));
                true
            }
            _ => false,
        }
    }

    fn merge_host_config(&mut self, config: Map<String, Value>) {
        if let Some(governance) = config.get("governance") {
            let mut governance = object(Some(governance));
            if let Some(rate_limits) = governance
                .get_mut("rate_limits")
                .and_then(|v| v.as_array_mut())
            {
                for rule in rate_limits {
                    if let Some(obj) = rule.as_object_mut() {
                        if let Some(window) = obj.get("window_ms").cloned() {
                            obj.insert(
                                "window_ms".into(),
                                Value::String(match window {
                                    Value::String(s) => s,
                                    other => other.to_string(),
                                }),
                            );
                        }
                    }
                }
            }
            self.config
                .insert("governance_policy".into(), Value::Object(governance));
        }
        if let Some(context_policy) = config.get("context_policy") {
            self.config
                .insert("context_policy".into(), context_policy.clone());
        }
        if let Some(signal_policy) = config.get("signal_policy") {
            let mut signal = object(Some(signal_policy));
            signal.remove("version");
            if let Some(ttl) = signal.get("ttl_ms").cloned() {
                signal.insert(
                    "ttl_ms".into(),
                    Value::String(match ttl {
                        Value::String(s) => s,
                        other => other.to_string(),
                    }),
                );
            }
            self.config
                .insert("signal_policy".into(), Value::Object(signal));
        }
        if let Some(scheduler_policy) = config.get("scheduler_policy") {
            let mut scheduler = object(Some(scheduler_policy));
            scheduler.remove("version");
            self.config
                .insert("scheduler_policy".into(), Value::Object(scheduler));
        }
        if let Some(quota) = config.get("resource_quota") {
            let mut quota = object(Some(quota));
            if let Some(window) = quota
                .get("memory_writes_per_window")
                .and_then(|v| v.as_array())
                .cloned()
            {
                quota.insert(
                    "memory_writes_per_window".into(),
                    json!({
                        "max_events": window.first().and_then(|v| v.as_u64()).unwrap_or(0),
                        "window_ms": window.get(1).map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
                    }),
                );
            }
            self.config
                .insert("resource_quota".into(), Value::Object(quota));
        }
        if let Some(grant) = config.get("budget_grant") {
            let mut grant = object(Some(grant));
            if let Some(tokens) = grant.get("tokens").cloned() {
                grant.insert(
                    "tokens".into(),
                    Value::String(match tokens {
                        Value::String(s) => s,
                        other => other.to_string(),
                    }),
                );
            }
            self.config
                .insert("budget_grant".into(), Value::Object(grant));
        }
        if let Some(prompt_budget) = config.get("prompt_budget") {
            self.context_policy()
                .insert("prompt_budget".into(), prompt_budget.clone());
        }
        if let Some(repeat_fuse) = config.get("repeat_fuse") {
            self.execution_policy()
                .insert("repeat_fuse".into(), repeat_fuse.clone());
        }
        if let Some(criteria_gate) = config.get("criteria_gate") {
            self.execution_policy()
                .insert("criteria_gate_enabled".into(), criteria_gate.clone());
        }
        if let Some(ratio) = config
            .get("knowledge_budget_ratio")
            .and_then(|v| v.as_f64())
        {
            self.context_policy().insert(
                "knowledge_budget_ppm".into(),
                json!((ratio * 1_000_000.0).round() as u64),
            );
        }
        if let Some(entropy) = config.get("entropy_watch") {
            let mut entropy = object(Some(entropy));
            if let Some(threshold) = entropy.remove("threshold").and_then(|v| v.as_f64()) {
                entropy.insert(
                    "threshold_ppm".into(),
                    json!((threshold * 1_000_000.0).round() as u64),
                );
            }
            if let Some(hysteresis) = entropy.remove("hysteresis").and_then(|v| v.as_f64()) {
                entropy.insert(
                    "hysteresis_ppm".into(),
                    json!((hysteresis * 1_000_000.0).round() as u64),
                );
            }
            self.execution_policy()
                .insert("entropy_watch".into(), Value::Object(entropy));
        }
        if let Some(gate) = config.get("tool_dispatch_gate") {
            self.feature_policy()
                .insert("tool_dispatch_gate".into(), gate.clone());
        }
        if let Some(reliability) = config.get("reliability") {
            let reliability = object(Some(reliability));
            let mut recovery = Map::new();
            if let Some(v) = reliability.get("provider_recovery_attempts") {
                recovery.insert("provider_recovery_attempts".into(), v.clone());
            }
            if let Some(v) = reliability.get("output_recovery_attempts") {
                recovery.insert("output_recovery_attempts".into(), v.clone());
            }
            if !recovery.is_empty() {
                self.config
                    .insert("recovery_policy".into(), Value::Object(recovery));
            }
            if let Some(max_input_bytes) = reliability.get("max_input_bytes") {
                let mut limits = object(self.config.get("kernel_limits"));
                limits.insert("max_input_bytes".into(), max_input_bytes.clone());
                self.config
                    .insert("kernel_limits".into(), Value::Object(limits));
            }
        }
    }

    fn workflow_spec(&self, raw: Map<String, Value>) -> Value {
        let nodes = raw
            .get("nodes")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let ids: Vec<String> = (0..nodes.len())
            .map(|index| format!("wf-node{index}"))
            .collect();
        let lowered: Vec<Value> = nodes
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let node = object(Some(value));
                let task = object(node.get("task"));
                let goal = if let Some(s) = node.get("task").and_then(|v| v.as_str()) {
                    s.to_string()
                } else {
                    task.get("goal")
                        .or_else(|| node.get("goal"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let dependencies = node
                    .get("depends_on")
                    .or_else(|| node.get("dependsOn"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let depends_on: Vec<Value> = dependencies
                    .into_iter()
                    .map(|dep| {
                        if let Some(n) = dep.as_u64() {
                            let idx = n as usize;
                            if idx < ids.len() {
                                return json!(ids[idx]);
                            }
                        }
                        dep
                    })
                    .collect();
                let mut run_spec_raw = Map::new();
                run_spec_raw.insert("goal".into(), json!(goal.clone()));
                if let Some(role) = node.get("role") {
                    run_spec_raw.insert("role".into(), role.clone());
                }
                if let Some(isolation) = node.get("isolation") {
                    run_spec_raw.insert("isolation".into(), isolation.clone());
                }
                let mut node_json = json!({
                    "node_id": ids[index],
                    "task": {
                        "goal": goal,
                    },
                    "run_spec": logical_run_spec(run_spec_raw, &string_field(&task, "goal")),
                });
                if let Some(criteria) = task.get("criteria") {
                    node_json["task"]
                        .as_object_mut()
                        .unwrap()
                        .insert("criteria".into(), criteria.clone());
                }
                if !depends_on.is_empty() {
                    node_json
                        .as_object_mut()
                        .unwrap()
                        .insert("depends_on".into(), Value::Array(depends_on));
                }
                node_json
            })
            .collect();
        json!({ "nodes": lowered })
    }
}

pub(crate) async fn canonical_kernel_apply(
    runtime: &mut CanonicalRunnerRuntime,
    pending: &mut Vec<KernelObservation>,
    event: Value,
) -> Result<()> {
    runtime.apply_host_event(event).await?;
    pending.extend(runtime.drain_host_observations());
    Ok(())
}

pub(crate) async fn canonical_kernel_action(
    runtime: &mut CanonicalRunnerRuntime,
    pending: &mut Vec<KernelObservation>,
    event: Value,
) -> Result<HostAction> {
    let action = runtime.apply_host_event(event).await?;
    pending.extend(runtime.drain_host_observations());
    action.ok_or_else(|| {
        Error::Other("kernel transition returned no action and no fault".to_string())
    })
}

pub(crate) fn canonical_action_from_planned_step(
    planned: &PlannedStep,
) -> Result<Option<HostAction>> {
    match &planned.disposition {
        StepDisposition::Terminal(terminal) => Ok(Some(HostAction {
            effect_id: String::new(),
            causation_id: String::new(),
            effect: HostEffect::Done {
                result: loop_result_from_terminal(&terminal.terminal)?,
            },
        })),
        StepDisposition::Effects(effects) => {
            if effects.effects.is_empty() {
                return Ok(None);
            }
            if effects.effects.len() != 1 {
                return Err(Error::Other(format!(
                    "Rust runner expects one canonical effect at a time, received {}",
                    effects.effects.len()
                )));
            }
            Ok(Some(protocol_action_from_wire(
                &effects.effects[0],
                &planned.observations,
            )?))
        }
    }
}

fn protocol_action_from_wire(
    effect: &WireKernelEffect,
    observations: &[KernelObservation],
) -> Result<HostAction> {
    let effect_id = effect.effect_id.as_str().to_string();
    let causation_id = effect.causation_input_id.as_str().to_string();
    let mapped = match &effect.effect {
        EffectKind::CallProvider(call) => HostEffect::CallProvider {
            context: rendered_context_from_wire(&call.context)?,
            tools: call
                .tools
                .iter()
                .map(|tool| ToolSchema {
                    name: CompactString::from(tool.name.as_str()),
                    description: tool.description.clone(),
                    parameters: tool.parameters.get().clone(),
                })
                .collect(),
        },
        EffectKind::ExecuteTools(execute) => HostEffect::ExecuteTool {
            calls: execute
                .calls
                .iter()
                .map(|call| ToolCall {
                    id: CompactString::from(call.call_id.as_str()),
                    name: CompactString::from(call.name.as_str()),
                    arguments: call.arguments.get().clone(),
                })
                .collect(),
        },
        EffectKind::RequestApproval(request) => HostEffect::RequestApproval {
            requests: request
                .requests
                .iter()
                .map(
                    |item| deepstrike_core::scheduler::state_machine::ApprovalRequest {
                        call_id: item.call_id.as_str().to_string(),
                        tool: item.tool_name.clone(),
                        arguments: item.arguments.get().clone(),
                        reason: item.reason.clone().unwrap_or_default(),
                    },
                )
                .collect(),
        },
        EffectKind::SpawnTasks(spawn) => HostEffect::SpawnWorkflow {
            nodes: spawn
                .tasks
                .iter()
                .map(|task| {
                    let role = task
                        .spec
                        .role
                        .and_then(|role| serde_json::to_value(role).ok())
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "custom".into());
                    let isolation = task
                        .spec
                        .isolation
                        .and_then(|isolation| serde_json::to_value(isolation).ok())
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "shared".into());
                    deepstrike_core::orchestration::workflow::WorkflowSpawnInfo {
                        agent_id: task.task_id.as_str().to_string(),
                        goal: task.spec.goal.clone(),
                        role,
                        isolation,
                        context_inheritance: "none".into(),
                        model_hint: None,
                        trust: "trusted".into(),
                        output_schema: None,
                        reducer: None,
                        input_agent_ids: Vec::new(),
                        judge_match: None,
                        loop_max_iters: None,
                        classify_labels: Vec::new(),
                        token_budget: None,
                        max_turns: None,
                        max_wall_ms: None,
                    }
                })
                .collect(),
            budget: None,
        },
        EffectKind::PreemptTasks(preempt) => HostEffect::PreemptSubAgents {
            agent_ids: preempt
                .attempts
                .iter()
                .map(|attempt| attempt.task_id.as_str().to_string())
                .collect(),
            reason: preempt.reason.clone(),
        },
        EffectKind::PersistMemory(persist) => HostEffect::PersistMemory {
            memory: memory_record_from_wire(&persist.memory),
        },
        EffectKind::QueryMemory(query) => HostEffect::QueryMemory {
            query: MemoryQuery {
                scope: MemoryScope::new(String::new(), String::new()),
                query: query.query.text.clone(),
                top_k: query.requested_k as usize,
                kinds: query
                    .query
                    .kinds
                    .iter()
                    .filter_map(|kind| {
                        serde_json::to_value(kind)
                            .ok()
                            .and_then(|v| serde_json::from_value(v).ok())
                    })
                    .collect(),
                min_score: None,
            },
            requested_k: query.requested_k as usize,
        },
        EffectKind::ArchivePageOut(archive) => {
            let archived =
                serde_json::from_str::<Vec<Message>>(&archive.payload.content).unwrap_or_default();
            let compressed = observations.iter().find_map(|obs| match obs {
                KernelObservation::Compressed {
                    action, summary, ..
                } => Some((*action, summary.clone())),
                _ => None,
            });
            let (pressure_action, summary) =
                compressed.unwrap_or((KernelPressureAction::MicroCompact, None));
            let tier = match pressure_action {
                KernelPressureAction::ContextCollapse | KernelPressureAction::AutoCompact => {
                    "semantic".to_string()
                }
                _ => "durable".to_string(),
            };
            HostEffect::ArchivePageOut {
                turn: 0,
                action: pressure_action,
                summary,
                archived,
                tier,
            }
        }
        EffectKind::LoadPayload(load) => HostEffect::LoadPayload {
            handle_id: load.handle_id.as_str().to_string(),
            payload_ref: load.payload_ref.as_str().to_string(),
        },
        EffectKind::EvaluateMilestone(eval) => HostEffect::EvaluateMilestone {
            phase_id: eval.request.phase_id.clone(),
            criteria: Vec::new(),
            verifier: None,
            required_evidence: Vec::new(),
        },
    };
    Ok(HostAction {
        effect_id,
        causation_id,
        effect: mapped,
    })
}

fn loop_result_from_terminal(terminal: &KernelTerminal) -> Result<LoopResult> {
    let usage_tokens = |usage: &deepstrike_core::runtime::kernel::wire::UsageReport| {
        usage
            .input_tokens
            .get()
            .saturating_add(usage.output_tokens.get())
    };
    match terminal {
        KernelTerminal::Agent(agent) => {
            let result = &agent.result;
            Ok(LoopResult {
                termination: map_termination(result.termination),
                final_message: result
                    .final_message
                    .as_ref()
                    .map(message_from_wire_provider)
                    .transpose()?,
                turns_used: result.turns_used,
                total_tokens_used: usage_tokens(&agent.usage),
                loop_continue: None,
                classify_branch: None,
                pace_decision: result.pace_decision.as_ref().map(|pace| PaceDecision {
                    action: match pace.action {
                        deepstrike_core::runtime::kernel::wire::PaceAction::Continue => {
                            PaceAction::Continue
                        }
                        deepstrike_core::runtime::kernel::wire::PaceAction::Sleep => {
                            PaceAction::Sleep
                        }
                        deepstrike_core::runtime::kernel::wire::PaceAction::Stop => {
                            PaceAction::Stop
                        }
                    },
                    delay_ms: pace.delay_ms.map(|v| v.get()),
                    reason: pace.reason.clone(),
                    coerced_from: pace.coerced_from.clone(),
                }),
                tournament_winner: None,
            })
        }
        KernelTerminal::Workflow(workflow) => Ok(LoopResult {
            termination: match workflow.outcome.status {
                deepstrike_core::runtime::kernel::wire::WorkflowStatus::Completed => {
                    TerminationReason::Completed
                }
                deepstrike_core::runtime::kernel::wire::WorkflowStatus::Failed => {
                    TerminationReason::Error
                }
                deepstrike_core::runtime::kernel::wire::WorkflowStatus::Cancelled => {
                    TerminationReason::UserAbort
                }
            },
            final_message: None,
            turns_used: workflow.usage.turns,
            total_tokens_used: usage_tokens(&workflow.usage),
            loop_continue: None,
            classify_branch: None,
            pace_decision: None,
            tournament_winner: None,
        }),
        KernelTerminal::Cancelled(cancelled) => Ok(LoopResult {
            termination: TerminationReason::UserAbort,
            final_message: None,
            turns_used: cancelled.usage.turns,
            total_tokens_used: usage_tokens(&cancelled.usage),
            loop_continue: None,
            classify_branch: None,
            pace_decision: None,
            tournament_winner: None,
        }),
        KernelTerminal::Failed(failed) => {
            let termination = if failed.failure.code
                == deepstrike_core::runtime::kernel::wire::KernelFailureCode::ProviderRecoveryExhausted
            {
                TerminationReason::ContextOverflow
            } else {
                TerminationReason::Error
            };
            Ok(LoopResult {
                termination,
                final_message: None,
                turns_used: failed.usage.turns,
                total_tokens_used: usage_tokens(&failed.usage),
                loop_continue: None,
                classify_branch: None,
                pace_decision: None,
                tournament_winner: None,
            })
        }
    }
}

fn map_termination(
    reason: deepstrike_core::runtime::kernel::wire::TerminationReason,
) -> TerminationReason {
    use deepstrike_core::runtime::kernel::wire::TerminationReason as Wire;
    match reason {
        Wire::Completed => TerminationReason::Completed,
        Wire::MaxTurns => TerminationReason::MaxTurns,
        Wire::TokenBudget => TerminationReason::TokenBudget,
        Wire::Deadline => TerminationReason::Timeout,
        Wire::ContextOverflow => TerminationReason::ContextOverflow,
        Wire::NoProgress => TerminationReason::NoProgress,
        Wire::MilestoneExceeded => TerminationReason::MilestoneExceeded,
    }
}

fn rendered_context_from_wire(
    context: &deepstrike_core::runtime::kernel::wire::RenderedContext,
) -> Result<RenderedContext> {
    let turns = context
        .turns
        .iter()
        .map(message_from_wire_provider)
        .collect::<Result<Vec<_>>>()?;
    let state_turn = context
        .state_turn
        .as_ref()
        .map(message_from_wire_provider)
        .transpose()?;
    let system_text = if context.system_knowledge.is_empty() {
        context.system_stable.clone()
    } else if context.system_stable.is_empty() {
        context.system_knowledge.clone()
    } else {
        format!("{}\n\n{}", context.system_stable, context.system_knowledge)
    };
    Ok(RenderedContext {
        system_text,
        system_stable: context.system_stable.clone(),
        system_knowledge: context.system_knowledge.clone(),
        turns,
        state_turn,
        frozen_prefix_len: context.frozen_prefix_len.map(|v| v as usize),
        budget_overflow: None,
    })
}

fn message_from_wire_provider(
    message: &deepstrike_core::runtime::kernel::wire::ProviderMessage,
) -> Result<Message> {
    Ok(Message {
        role: match message.role {
            deepstrike_core::runtime::kernel::wire::MessageRole::System => Role::System,
            deepstrike_core::runtime::kernel::wire::MessageRole::User => Role::User,
            deepstrike_core::runtime::kernel::wire::MessageRole::Assistant => Role::Assistant,
            deepstrike_core::runtime::kernel::wire::MessageRole::Tool => Role::Tool,
        },
        content: Content::Text(message.content.clone()),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| ToolCall {
                id: CompactString::from(call.call_id.as_str()),
                name: CompactString::from(call.name.as_str()),
                arguments: call.arguments.get().clone(),
            })
            .collect(),
        token_count: message.tokens,
    })
}

fn memory_record_from_wire(
    write: &deepstrike_core::runtime::kernel::wire::CanonicalMemoryWrite,
) -> MemoryRecord {
    let kind = serde_json::to_value(&write.kind)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or(MemoryKind::Reference);
    let now = write.accepted_at_ms.get();
    MemoryRecord {
        record_id: format!("memory:{}", uuid::Uuid::new_v4()),
        scope: MemoryScope::new(String::new(), String::new()),
        name: write.name.clone(),
        kind,
        content: write.content.clone(),
        description: write.description.clone(),
        provenance: MemoryProvenance {
            session_id: None,
            author: MemoryAuthor::Model,
            trust: MemoryTrustLevel::Untrusted,
            evidence_refs: write.evidence_refs.clone(),
        },
        created_at: now,
        updated_at: now,
        last_recalled_at: None,
        recall_count: 0,
        confidence: 1.0,
        links: Vec::new(),
        pinned: false,
        ttl_days: None,
    }
}

fn logical_run_spec(raw: Map<String, Value>, goal: &str) -> Value {
    let mut out = Map::new();
    out.insert(
        "goal".into(),
        json!(raw.get("goal").and_then(|v| v.as_str()).unwrap_or(goal)),
    );
    for key in [
        "role",
        "isolation",
        "verification_contract_id",
        "exposure_baseline",
        "metadata",
        "capability_filter",
        "loop_round",
    ] {
        if let Some(value) = raw.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}

fn canonical_signal(event: &Value) -> Value {
    let signal = object(event.get("signal"));
    let delivery_id = event
        .get("delivery_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let payload = if delivery_id.starts_with("injected-") {
        if let Some(summary) = signal.get("summary").filter(|v| v.is_string()) {
            summary.clone()
        } else {
            signal.get("payload").cloned().unwrap_or_else(|| json!({}))
        }
    } else {
        signal.get("payload").cloned().unwrap_or_else(|| json!({}))
    };
    let mut wire_signal = json!({
        "signal_id": signal.get("signal_id").or_else(|| signal.get("id")).and_then(|v| v.as_str()).unwrap_or(&uuid::Uuid::new_v4().to_string()),
        "target": if let Some(recipient) = signal.get("recipient").and_then(|v| v.as_str()) {
            json!({ "kind": "task", "task_id": recipient })
        } else {
            json!({ "kind": "operation" })
        },
        "payload": payload,
    });
    if let Some(source) = signal.get("source") {
        wire_signal
            .as_object_mut()
            .unwrap()
            .insert("source".into(), source.clone());
    }
    if let Some(urgency) = signal.get("urgency") {
        wire_signal
            .as_object_mut()
            .unwrap()
            .insert("urgency".into(), urgency.clone());
    }
    if let Some(ts) = signal.get("timestamp_ms") {
        wire_signal.as_object_mut().unwrap().insert(
            "source_timestamp_ms".into(),
            Value::String(match ts {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            }),
        );
    }
    if let Some(dedupe) = signal.get("dedupe_key") {
        wire_signal
            .as_object_mut()
            .unwrap()
            .insert("dedupe_key".into(), dedupe.clone());
    }
    json!({
        "kind": "deliver_signal",
        "delivery_id": delivery_id,
        "attempt": event.get("attempt").and_then(|v| v.as_u64()).unwrap_or(1),
        "signal": wire_signal,
    })
}

fn canonical_capability_command(command: Map<String, Value>) -> Value {
    let capability = object(command.get("capability"));
    if command.get("action").and_then(|v| v.as_str()) == Some("mount") {
        json!({
            "kind": "apply_capability_patch",
            "patch": {
                "mount": [{
                    "kind": capability.get("kind").and_then(|v| v.as_str()).unwrap_or("tool"),
                    "id": string_field(&capability, "id"),
                    "description": capability.get("description").cloned().unwrap_or(Value::Null),
                }],
            },
        })
    } else {
        json!({
            "kind": "apply_capability_patch",
            "patch": {
                "unmount": [{
                    "kind": command.get("kind").and_then(|v| v.as_str()).unwrap_or("tool"),
                    "id": string_field(&command, "id"),
                }],
            },
        })
    }
}

fn initial_message(raw: Map<String, Value>) -> Value {
    if raw.get("content").map(|v| v.is_array()).unwrap_or(false) {
        return json!({
            "role": raw.get("role").and_then(|v| v.as_str()).unwrap_or("user"),
            "content": raw.get("content").cloned().unwrap_or(json!("")),
            "tokens": raw.get("token_count").cloned(),
        });
    }
    let mut message = provider_message(&raw);
    message.as_object_mut().unwrap().remove("tool_calls");
    message
}

fn provider_message(raw: &Map<String, Value>) -> Value {
    let content = match raw.get("content") {
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(other) => Value::String(other.to_string()),
        None => Value::String(String::new()),
    };
    let mut message = json!({
        "role": raw.get("role").and_then(|v| v.as_str()).unwrap_or("assistant"),
        "content": content,
    });
    if let Some(calls) = raw.get("tool_calls").and_then(|v| v.as_array()) {
        let tool_calls: Vec<Value> = calls
            .iter()
            .map(|call| {
                let call = object(Some(call));
                json!({
                    "call_id": call.get("call_id").or_else(|| call.get("id")).and_then(|v| v.as_str()).unwrap_or(""),
                    "name": string_field(&call, "name"),
                    "arguments": call.get("arguments").cloned().unwrap_or_else(|| json!({})),
                })
            })
            .collect();
        message
            .as_object_mut()
            .unwrap()
            .insert("tool_calls".into(), Value::Array(tool_calls));
    }
    message
}

fn provider_stop_reason(value: Option<&Value>) -> Option<String> {
    let reason = value.and_then(|v| v.as_str())?.to_ascii_lowercase();
    if reason.is_empty() {
        return None;
    }
    Some(match reason.as_str() {
        "end_turn" | "tool_use" | "max_tokens" | "stop_sequence" | "content_filter" => reason,
        _ => "other".into(),
    })
}

fn regex_context_overflow(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context")
        || lower.contains("token") && lower.contains("limit")
        || lower.contains("too long")
}

fn object(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

fn string_field(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn string_value(value: &Value, key: &str) -> String {
    string_field(&object(Some(value)), key)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn sha256_digest(value: &str) -> String {
    canonical_digest(value.as_bytes()).as_str().to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use deepstrike_core::runtime::kernel::KernelObservation;
    use serde_json::json;

    use super::{CanonicalRunnerOptions, CanonicalRunnerRuntime};
    use crate::runtime::canonical_kernel::{CanonicalKernel, EffectKind, WireEnvelope};
    use crate::runtime::canonical_kernel_step::CanonicalKernelHost;
    use crate::runtime::host_projection::HostEffect;
    use crate::runtime::kernel_journal::{InMemoryKernelJournal, KernelJournal};

    fn test_options() -> CanonicalRunnerOptions {
        CanonicalRunnerOptions {
            max_context_tokens: 128_000,
            max_turns: None,
            max_total_tokens: None,
            max_wall_ms: None,
            memory_binding_id: "test-binding".into(),
            persist_payload: None,
        }
    }

    #[tokio::test]
    async fn workflow_spawn_result_uses_kernel_issued_attempt_ids() {
        let journal: Arc<dyn KernelJournal> = Arc::new(InMemoryKernelJournal::new());
        let mut runtime = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            journal.clone(),
            "op-spawn-attempts",
            test_options(),
        )
        .expect("runtime");

        let spec = deepstrike_core::orchestration::workflow::WorkflowSpec::new(vec![
            deepstrike_core::orchestration::workflow::WorkflowNode::new(
                deepstrike_core::types::task::RuntimeTask::new("do the thing"),
                deepstrike_core::types::agent::AgentRole::Implement,
            ),
        ]);
        let action = runtime
            .start_workflow_value(serde_json::to_value(spec).expect("workflow spec"))
            .await
            .expect("transition")
            .expect("action");

        let pending = runtime.host.pending_effects();
        let spawn = pending
            .iter()
            .find_map(|e| {
                if let EffectKind::SpawnTasks(spawn) = &e.effect {
                    Some(spawn)
                } else {
                    None
                }
            })
            .expect("a spawn effect is pending");
        assert_eq!(spawn.tasks.len(), 1);
        let task_id = spawn.tasks[0].task_id.as_str().to_string();
        let kernel_attempt_id = spawn.tasks[0].attempt_id.as_str().to_string();
        assert!(
            !kernel_attempt_id.is_empty(),
            "kernel must assign a non-empty attempt_id"
        );

        runtime
            .apply_host_event(json!({
                "kind": "workflow_spawn_result",
                "effect_id": action.effect_id,
                "started_agent_ids": [&task_id],
            }))
            .await
            .expect("resolve spawn");

        assert!(
            runtime.host.pending_effects().is_empty(),
            "spawn effect should be resolved"
        );
        assert_eq!(runtime.local_subagents_spawned(), 1);
        assert_eq!(
            runtime.host.attempt_id(&task_id).as_deref(),
            Some(kernel_attempt_id.as_str())
        );

        drop(runtime);
        let mut restored = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            journal,
            "op-spawn-attempts",
            test_options(),
        )
        .expect("restored runtime");
        restored.restore().await.expect("restore");
        assert_eq!(
            restored.host.attempt_id(&task_id).as_deref(),
            Some(kernel_attempt_id.as_str()),
            "checkpoint/journal restore must preserve the kernel-issued attempt"
        );
        assert_eq!(
            restored.local_subagents_spawned(),
            1,
            "restart must project the kernel-owned spawn count"
        );

        restored
            .apply_host_event(json!({
                "kind": "sub_agent_completed",
                "result": {
                    "agent_id": task_id,
                    "result": {
                        "termination": "completed",
                        "final_message": null,
                        "turns_used": 1,
                        "total_tokens_used": 1
                    }
                }
            }))
            .await
            .expect("resolve child completion after restore");

        // Drain any observations produced by the resolution.
        let _ = restored.drain_host_observations();
    }

    #[tokio::test]
    async fn restore_publishes_observations_from_a_staged_durable_replay() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json"
        ))
        .expect("fixture");
        let configure: WireEnvelope =
            serde_json::from_value(fixture["links"][0]["envelope"].clone())
                .expect("configure envelope");
        let start: WireEnvelope = serde_json::from_value(fixture["links"][1]["envelope"].clone())
            .expect("start envelope");
        let journal: Arc<dyn KernelJournal> = Arc::new(InMemoryKernelJournal::new());
        let writer = CanonicalKernelHost::new(
            CanonicalKernel::default(),
            journal.clone(),
            configure.operation_id.as_str(),
        )
        .expect("writer");

        writer.transition(configure).await.expect("configure");
        writer.transition(start.clone()).await.expect("start");
        journal
            .stage_outbound_envelope(
                start.operation_id.as_str(),
                &serde_json::to_string(&start).expect("serialize staged envelope"),
            )
            .await
            .expect("stage replay");

        let mut restored = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            journal,
            start.operation_id.as_str(),
            test_options(),
        )
        .expect("restored runtime");
        restored.restore().await.expect("restore");

        assert!(
            restored
                .drain_host_observations()
                .iter()
                .any(|observation| matches!(
                    observation,
                    KernelObservation::CheckpointTaken { .. }
                )),
            "the restarted runner must publish observations from the staged durable transition"
        );
        assert!(restored.resume_action().expect("resume action").is_some());
    }

    #[tokio::test]
    async fn restore_projects_turn_and_messages_from_canonical_state() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/kernel-wire/golden_lifecycle_agent_full_turn.json"
        ))
        .expect("fixture");
        let envelopes: Vec<WireEnvelope> = fixture["links"]
            .as_array()
            .expect("fixture links")
            .iter()
            .map(|link| serde_json::from_value(link["envelope"].clone()).expect("fixture envelope"))
            .collect();
        let operation_id = envelopes[0].operation_id.as_str().to_string();
        let journal: Arc<dyn KernelJournal> = Arc::new(InMemoryKernelJournal::new());
        let writer =
            CanonicalKernelHost::new(CanonicalKernel::default(), journal.clone(), &operation_id)
                .expect("writer");
        for envelope in envelopes {
            writer.transition(envelope).await.expect("transition");
        }

        let mut restored = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            journal,
            operation_id,
            test_options(),
        )
        .expect("restored runtime");
        restored.restore().await.expect("restore");

        assert_eq!(restored.turn(), 1);
        assert!(
            restored
                .drain_new_messages()
                .iter()
                .any(|message| message.role == deepstrike_core::types::message::Role::Assistant),
            "restart must project messages from canonical context state"
        );
    }

    #[tokio::test]
    async fn load_payload_has_a_dedicated_host_action_after_restore() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/kernel-wire/golden_lifecycle_external_payload.json"
        ))
        .expect("fixture");
        let envelopes: Vec<WireEnvelope> = fixture["links"]
            .as_array()
            .expect("fixture links")
            .iter()
            .take(5)
            .map(|link| serde_json::from_value(link["envelope"].clone()).expect("fixture envelope"))
            .collect();
        let operation_id = envelopes[0].operation_id.as_str().to_string();
        let journal: Arc<dyn KernelJournal> = Arc::new(InMemoryKernelJournal::new());
        let writer =
            CanonicalKernelHost::new(CanonicalKernel::default(), journal.clone(), &operation_id)
                .expect("writer");
        for envelope in envelopes {
            writer.transition(envelope).await.expect("transition");
        }

        let mut restored = CanonicalRunnerRuntime::new(
            CanonicalKernel::default(),
            journal,
            operation_id,
            test_options(),
        )
        .expect("restored runtime");
        restored.restore().await.expect("restore");
        let action = restored
            .resume_action()
            .expect("projection")
            .expect("action");
        assert!(matches!(
            &action.effect,
            HostEffect::LoadPayload {
                handle_id,
                payload_ref,
            } if handle_id == "call-1" && payload_ref == "payload:01J8Y2QK7C4N0V"
        ));
        restored
            .apply_host_event(json!({
                "kind": "payload_loaded",
                "effect_id": action.effect_id,
                "handle_id": "call-1",
                "content": "the full report body, far larger than this operation keeps resident, repeated so it clears the inline threshold by a comfortable margin",
                "digest": "sha256:720fdd2a3796213072f120b7217adf73b7cc85a39f2d6dffdd605f9945a6de2a",
                "original_size": 135,
            }))
            .await
            .expect("payload resolution");
        assert!(
            restored
                .host
                .pending_effects()
                .iter()
                .all(|effect| !matches!(&effect.effect, EffectKind::LoadPayload(_)))
        );
    }
}
