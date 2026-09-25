//! High-level Rust agent/session facade.
//!
//! `RuntimeRunner` remains the fully configurable kernel driver.  This module provides the
//! stable object model used by the other SDKs while keeping Rust ownership and streaming idioms:
//! an [`Agent`] owns one runner, and an [`AgentSession`] gives that runner a durable session id.

use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::run_event::RunEvent;
use crate::runtime::session_log::SessionEntry;
use crate::{MemoryQuery, MemoryRecall, MemoryRecord, Result, RuntimeOptions, RuntimeRunner};

/// Rust-native workflow handle. It exposes the canonical DAG state machine without imposing an
/// executor or task runtime; hosts decide how to run each returned spawn descriptor.
pub struct AgentWorkflow {
    run: deepstrike_core::orchestration::workflow::WorkflowRun,
}

impl AgentWorkflow {
    pub fn new(spec: &deepstrike_core::orchestration::workflow::WorkflowSpec) -> Result<Self> {
        Ok(Self {
            run: deepstrike_core::orchestration::workflow::WorkflowRun::new(spec)
                .map_err(|e| crate::Error::Other(e.to_string()))?,
        })
    }

    pub fn ready_batch(&mut self) -> Vec<usize> {
        self.run.expand_ready_controllers();
        self.run.ready_batch()
    }

    pub fn spawn_info(
        &self,
        node: usize,
    ) -> deepstrike_core::orchestration::workflow::WorkflowSpawnInfo {
        self.run.spawn_info(node)
    }

    pub fn mark_spawned(&mut self, node: usize, agent_id: &str) {
        self.run.mark_spawned(node, agent_id);
    }

    pub fn mark_denied(&mut self, node: usize) {
        self.run.mark_denied(node);
    }

    pub fn record_completion(
        &mut self,
        agent_id: &str,
        result: deepstrike_core::types::result::LoopResult,
    ) -> Option<usize> {
        self.run.record_completion(agent_id, result)
    }

    pub fn is_complete(&self) -> bool {
        self.run.is_complete()
    }

    pub fn outcomes(&self) -> Vec<deepstrike_core::orchestration::workflow::WorkflowNodeOutcome> {
        self.run.node_outcomes()
    }
}

/// Per-run inputs that are intentionally separate from `RuntimeOptions`.
/// This keeps a reusable `Agent` immutable while allowing each session turn to carry its own
/// criteria, extensions, and multimodal attachments.
#[derive(Debug, Clone, Default)]
pub struct AgentRunOptions {
    pub criteria: Vec<String>,
    pub extensions: Option<serde_json::Value>,
    pub attachments: Vec<deepstrike_core::types::message::ContentPart>,
    pub output_schema: Option<serde_json::Value>,
}

/// A completed run projected from the runtime event stream.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRunResult {
    pub text: String,
    pub run_id: Option<String>,
    pub session_id: String,
    pub status: String,
    pub iterations: u32,
    pub total_tokens: u64,
    pub usage: Option<AgentUsage>,
    pub evidence: Option<AgentEvidence>,
    pub output_validation: Option<OutputValidation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputValidation {
    pub ok: bool,
    pub errors: Vec<String>,
}

fn validate_output(text: &str, schema: &serde_json::Value) -> OutputValidation {
    let mut value = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(error) => {
            return OutputValidation {
                ok: false,
                errors: vec![format!("structured output is not valid JSON: {error}")],
            };
        }
    };
    match crate::tools::validate_tool_arguments(schema, &mut value) {
        Ok(_) => OutputValidation {
            ok: true,
            errors: Vec::new(),
        },
        Err(error) => OutputValidation {
            ok: false,
            errors: vec![error],
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentEvidence {
    pub context_binding: Option<serde_json::Value>,
    pub route: Option<serde_json::Value>,
    pub measurement: Option<serde_json::Value>,
}

/// A high-level agent backed by the canonical Rust runtime.
pub struct Agent {
    runner: Arc<RuntimeRunner>,
}

impl Agent {
    pub fn new(options: RuntimeOptions) -> Self {
        Self {
            runner: Arc::new(RuntimeRunner::new(options)),
        }
    }

    pub fn with_runner(runner: RuntimeRunner) -> Self {
        Self {
            runner: Arc::new(runner),
        }
    }

    pub fn runner(&self) -> &RuntimeRunner {
        &self.runner
    }

    pub fn session(&self, session_id: impl Into<String>) -> AgentSession {
        AgentSession {
            runner: Arc::clone(&self.runner),
            session_id: session_id.into(),
        }
    }

    pub async fn run(&self, goal: &str) -> Result<AgentRunResult> {
        self.session(uuid::Uuid::new_v4().to_string())
            .run(goal)
            .await
    }

    pub async fn stream<'a>(&'a self, goal: &'a str) -> Result<AgentStream<'a>> {
        self.runner.run_streaming(goal, &[], None, None).await
    }

    pub async fn run_with_options(
        &self,
        goal: &str,
        options: &AgentRunOptions,
    ) -> Result<AgentRunResult> {
        self.session(uuid::Uuid::new_v4().to_string())
            .run_with_options(goal, options)
            .await
    }

    pub async fn listen(&self, session_id: impl Into<String>) -> Result<Option<AgentRunResult>> {
        let session = self.session(session_id);
        let Some(text) = session.runner.listen(&session.session_id).await? else {
            return Ok(None);
        };
        Ok(Some(AgentRunResult {
            text,
            run_id: session.latest_run_id().await?,
            session_id: session.session_id.clone(),
            status: "completed".into(),
            ..Default::default()
        }))
    }
}

/// A durable, resumable session handle.
#[derive(Clone)]
pub struct AgentSession {
    runner: Arc<RuntimeRunner>,
    session_id: String,
}

pub type AgentStream<'a> = Pin<Box<dyn Stream<Item = Result<RunEvent>> + 'a>>;

impl AgentSession {
    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub async fn run(&self, goal: &str) -> Result<AgentRunResult> {
        self.run_with_options(goal, &AgentRunOptions::default())
            .await
    }

    pub async fn run_with_options(
        &self,
        goal: &str,
        options: &AgentRunOptions,
    ) -> Result<AgentRunResult> {
        let mut stream = self.stream_with_options(goal, options).await?;
        let mut result = AgentRunResult {
            session_id: self.session_id.clone(),
            ..Default::default()
        };
        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(delta) => result.text.push_str(&delta),
                RunEvent::Done {
                    iterations,
                    total_tokens,
                    status,
                } => {
                    result.iterations = iterations;
                    result.total_tokens = total_tokens;
                    result.status = status;
                }
                _ => {}
            }
        }
        result.run_id = self.latest_run_id().await?;
        result.usage = Some(AgentUsage {
            total_tokens: result.total_tokens,
        });
        result.evidence = self.latest_evidence().await?;
        if let Some(schema) = &options.output_schema {
            result.output_validation = Some(validate_output(&result.text, schema));
        }
        Ok(result)
    }

    pub async fn stream<'a>(&'a self, goal: &'a str) -> Result<AgentStream<'a>> {
        self.runner
            .run_streaming(goal, &[], None, Some(&self.session_id))
            .await
    }

    pub async fn stream_with_options<'a>(
        &'a self,
        goal: &'a str,
        options: &'a AgentRunOptions,
    ) -> Result<AgentStream<'a>> {
        self.runner
            .run_streaming_with_attachments(
                goal,
                &options.criteria,
                options.extensions.as_ref(),
                Some(&self.session_id),
                &options.attachments,
            )
            .await
    }

    pub async fn resume<'a>(&'a self) -> Result<AgentStream<'a>> {
        self.runner.wake_streaming(&self.session_id, None).await
    }

    pub async fn history(&self) -> Result<Vec<SessionEntry>> {
        self.runner.read_session(&self.session_id).await
    }

    /// Reconstruct the current message context from the durable session projection.
    pub async fn replay_messages(
        &self,
    ) -> Result<Vec<deepstrike_core::types::message::CoreMessage>> {
        Ok(crate::runtime::replay::replay_messages(
            &self.history().await?,
        ))
    }

    /// Return the provider replay messages recorded for this session.
    pub async fn recorded_messages(
        &self,
    ) -> Result<Vec<deepstrike_core::types::message::CoreMessage>> {
        Ok(
            crate::runtime::replay_fixture::extract_recorded_messages_from_entries(
                &self.history().await?,
            ),
        )
    }

    pub async fn is_mid_run(&self) -> Result<bool> {
        Ok(crate::runtime::replay::is_mid_run(&self.history().await?))
    }

    pub async fn latest_seq(&self) -> Result<i64> {
        self.runner.latest_session_seq(&self.session_id).await
    }

    pub async fn remember(&self, memory: MemoryRecord) -> Result<()> {
        self.runner
            .write_memory(memory, Some(&self.session_id), None)
            .await
    }

    pub async fn recall(&self, query: MemoryQuery) -> Result<Vec<MemoryRecall>> {
        self.runner
            .query_memory(query, Some(&self.session_id), None)
            .await
    }

    pub fn interrupt(&self) {
        self.runner.interrupt();
    }

    async fn latest_run_id(&self) -> Result<Option<String>> {
        Ok(self.history().await?.into_iter().rev().find_map(|entry| {
            if let deepstrike_core::runtime::session::SessionEvent::RunStarted { run_id, .. } =
                entry.event
            {
                Some(run_id)
            } else {
                None
            }
        }))
    }

    async fn latest_evidence(&self) -> Result<Option<AgentEvidence>> {
        let entry = self.history().await?.into_iter().rev().find(|entry| {
            matches!(
                entry.event,
                deepstrike_core::runtime::session::SessionEvent::ContextPrepared { .. }
            )
        });
        let Some(entry) = entry else {
            return Ok(None);
        };
        let deepstrike_core::runtime::session::SessionEvent::ContextPrepared {
            preparation, ..
        } = entry.event
        else {
            unreachable!();
        };
        Ok(Some(AgentEvidence {
            context_binding: serde_json::to_value(preparation.binding).ok(),
            route: Some(preparation.provider_route),
            measurement: serde_json::to_value(preparation.prompt_measurement).ok(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::validate_output;

    #[test]
    fn structured_output_validation_is_non_fatal() {
        let schema = serde_json::json!({"type": "object", "required": ["answer"]});
        assert!(validate_output(r#"{"answer":"ok"}"#, &schema).ok);
        let invalid = validate_output("not-json", &schema);
        assert!(!invalid.ok);
        assert!(invalid.errors[0].contains("not valid JSON"));
    }
}
