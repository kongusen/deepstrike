use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_stream::try_stream;
use deepstrike_core::memory::idle_pipeline::{IdleAction, IdleEvent, IdlePipeline, IdlePolicy};
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::scheduler::state_machine::{LoopAction, LoopEvent, LoopObservation, LoopStateMachine};
use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::message::{Message, ToolCall};
use deepstrike_core::types::signal::{RuntimeSignal as KernelSignal, SignalSource as KernelSignalSource, SignalType as KernelSignalType, Urgency};
use deepstrike_core::types::policy::SignalDisposition;
use deepstrike_core::types::skill::SkillMetadata;
use deepstrike_core::types::task::RuntimeTask;
use futures::StreamExt;

use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::{DreamResult, DreamStore};
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::execution_plane::{
    ExecutionPlane, LocalExecutionPlane, RunContext, ToolSuspendHandler,
};
use crate::runtime::replay::{is_mid_run, replay_messages};
use crate::runtime::provider_replay::{peek_provider_replay, seed_provider_replay_from_events};
use crate::runtime::session_log::{SessionEntry, SessionLog};
use crate::signals::SignalSource;
use crate::{Error, Result};

/// Configuration for a `RuntimeRunner` (aligned with Node/Python `RuntimeOptions`).
pub struct RuntimeOptions {
    pub provider: Box<dyn LLMProvider>,
    pub execution_plane: Option<Box<dyn ExecutionPlane>>,
    pub session_log: Option<Arc<dyn SessionLog>>,
    /// When set, `execute` reuses this session id.
    pub session_id: Option<String>,
    pub max_tokens: u32,
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub extensions: Option<serde_json::Value>,
    pub agent_id: Option<String>,
    pub system_prompt: Option<String>,
    pub initial_memory: Vec<String>,
    pub skill_dir: Option<std::path::PathBuf>,
    pub dream_store: Option<Box<dyn DreamStore>>,
    pub knowledge_source: Option<Box<dyn KnowledgeSource>>,
    pub signal_source: Option<Box<dyn SignalSource>>,
    pub governance: Option<Arc<tokio::sync::Mutex<Governance>>>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
}

/// Orchestrates the agentic turn loop via `LoopStateMachine` + session event log.
pub struct RuntimeRunner {
    opts: RuntimeOptions,
    plane: Box<dyn ExecutionPlane>,
    interrupted: AtomicBool,
}

impl RuntimeRunner {
    pub fn new(mut opts: RuntimeOptions) -> Self {
        let plane = opts
            .execution_plane
            .take()
            .unwrap_or_else(|| Box::new(LocalExecutionPlane::new()));
        Self {
            opts,
            plane,
            interrupted: AtomicBool::new(false),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Relaxed);
    }

    pub fn execution_plane(&self) -> &dyn ExecutionPlane {
        self.plane.as_ref()
    }

    pub async fn execute(&self, goal: &str) -> Result<String> {
        collect_text(self.run_streaming(goal, &[], None, None).await?).await
    }

    pub async fn execute_with_criteria(&self, goal: &str, criteria: &[String]) -> Result<String> {
        collect_text(self.run_streaming(goal, criteria, None, None).await?).await
    }

    pub async fn run_streaming<'a>(
        &'a self,
        goal: &'a str,
        criteria: &'a [String],
        extensions: Option<&'a serde_json::Value>,
        session_id: Option<&'a str>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        let session_id = session_id
            .map(str::to_string)
            .or_else(|| self.opts.session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let prior = self.read_entries(&session_id).await?;
        let mid_run = is_mid_run(&prior);

        if !mid_run {
            self.log(
                &session_id,
                SessionEvent::RunStarted {
                    run_id: uuid::Uuid::new_v4().to_string(),
                    goal: goal.to_string(),
                    criteria: criteria.to_vec(),
                    agent_id: self.opts.agent_id.clone(),
                    system_prompt: self.opts.system_prompt.clone(),
                },
            )
            .await;
        }

        let goal_owned = goal.to_string();
        let criteria_owned = criteria.to_vec();
        let extensions_owned = extensions.cloned();
        let prior_events = if prior.is_empty() { None } else { Some(prior) };

        Ok(Box::pin(self.execute_inner(
            session_id,
            goal_owned,
            criteria_owned,
            extensions_owned,
            prior_events,
            mid_run,
        )))
    }

    pub async fn wake_streaming(
        &self,
        session_id: &str,
        extensions: Option<&serde_json::Value>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + '_>>> {
        let prior = self.read_entries(session_id).await?;
        if prior
            .iter()
            .any(|e| matches!(e.event, SessionEvent::RunTerminal { .. }))
        {
            return Ok(Box::pin(futures::stream::empty()));
        }
        let start = prior
            .iter()
            .rev()
            .find(|e| matches!(e.event, SessionEvent::RunStarted { .. }))
            .ok_or_else(|| Error::Other(format!("no run_started for session: {session_id}")))?;

        let (goal, criteria) = match &start.event {
            SessionEvent::RunStarted { goal, criteria, .. } => (goal.clone(), criteria.clone()),
            _ => unreachable!(),
        };

        Ok(Box::pin(self.execute_inner(
            session_id.to_string(),
            goal,
            criteria,
            extensions.cloned(),
            Some(prior),
            true,
        )))
    }

    pub async fn wake(&self, session_id: &str) -> Result<String> {
        collect_text(self.wake_streaming(session_id, None).await?).await
    }

    pub async fn dream(&self, agent_id: &str, now_ms: u64) -> Result<DreamResult> {
        let store = self
            .opts
            .dream_store
            .as_ref()
            .ok_or_else(|| Error::Other("dream_store not configured".into()))?;

        let sessions = store.load_sessions(agent_id).await?;
        let existing_memories = store.load_memories(agent_id).await?;
        if sessions.is_empty() {
            return Ok(DreamResult::default());
        }

        let policy = IdlePolicy::new(agent_id);
        let mut pipeline = IdlePipeline::new(policy);

        let messages = match pipeline.feed(IdleEvent::Trigger {
            sessions,
            existing_memories: existing_memories.clone(),
            now_ms,
        }) {
            IdleAction::SynthesizeInsights { messages } => messages,
            IdleAction::Noop => return Ok(DreamResult::default()),
            _ => return Err(Error::Other("unexpected IdlePipeline::Trigger action".into())),
        };

        let mut synthesis_text = String::new();
        let context = rendered_context_from_messages(messages);
        let synth_state = self.opts.provider.create_run_state();
        let mut stream = self
            .opts
            .provider
            .stream(&context, &[], None, synth_state.as_ref())
            .await?;
        while let Some(evt) = stream.next().await {
            if let Ok(StreamEvent::TextDelta { delta }) = evt {
                synthesis_text.push_str(&delta);
            }
        }

        let (curation_result, run_result) =
            match pipeline.feed(IdleEvent::SynthesisResult { content: synthesis_text }) {
                IdleAction::CommitMemories { result, run_result, .. } => (result, run_result),
                _ => {
                    return Err(Error::Other(
                        "unexpected IdlePipeline::SynthesisResult action".into(),
                    ))
                }
            };

        let entries_added = curation_result.stats.entries_added;
        let entries_removed = curation_result.to_remove_indices.len();
        store
            .commit(agent_id, curation_result, &existing_memories)
            .await?;

        Ok(DreamResult {
            sessions_processed: run_result.sessions_processed,
            insights_extracted: run_result.insights_extracted,
            entries_added,
            entries_removed,
        })
    }

    fn execute_inner(
        &self,
        session_id: String,
        goal: String,
        criteria: Vec<String>,
        extensions: Option<serde_json::Value>,
        prior_events: Option<Vec<SessionEntry>>,
        resume_mid_run: bool,
    ) -> impl futures::Stream<Item = Result<RunEvent>> + '_ {
        try_stream! {
            self.interrupted.store(false, Ordering::Relaxed);

            if let Some(ks) = &self.opts.knowledge_source {
                ks.init().await?;
            }

            let provider_policy = self.opts.provider.runtime_policy();
            let effective_max_turns = self.opts.max_turns.or(provider_policy.max_turns).unwrap_or(25);
            let effective_timeout = self.opts.timeout_ms.or(provider_policy.timeout_ms);

            let policy = LoopPolicy {
                max_tokens: self.opts.max_tokens,
                max_turns: effective_max_turns,
                timeout_ms: effective_timeout,
                ..Default::default()
            };

            let mut sm = LoopStateMachine::new(policy);
            sm.tools = self.plane.schemas();

            if self.opts.dream_store.is_some() && self.opts.agent_id.is_some() {
                sm.ctx.set_memory_enabled(true);
            }
            if self.opts.knowledge_source.is_some() {
                sm.ctx.set_knowledge_enabled(true);
            }

            if let Some(sp) = &self.opts.system_prompt {
                let tokens = ((sp.len() / 4) as u32).max(1);
                sm.ctx.partitions.system.push(Message::system(sp.clone()), tokens);
            }
            for mem in &self.opts.initial_memory {
                let tokens = ((mem.len() / 4) as u32).max(1);
                sm.ctx.partitions.memory.push(Message::user(mem.clone()), tokens);
            }

            if let Some(skill_dir) = &self.opts.skill_dir {
                if let Ok(entries) = std::fs::read_dir(skill_dir) {
                    let mut metas = Vec::new();
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("md") {
                            if let Ok(content) = std::fs::read_to_string(&path) {
                                let name = path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                let description = parse_frontmatter_description(&content);
                                metas.push(SkillMetadata::new(name, description));
                            }
                        }
                    }
                    sm.ctx.set_available_skills(metas);
                }
            }

            if let Some(events) = &prior_events {
                seed_provider_replay_from_events(self.opts.provider.as_ref(), events);
                sm.preload_history(replay_messages(events));
            }

            let ext = merge_extensions(self.opts.extensions.as_ref(), extensions.as_ref());
            let provider_state = self.opts.provider.create_run_state();
            let mut router = SignalRouter::new(256);
            let mut next_archive_start = next_archived_seq_start(prior_events.as_deref());
            let session_start_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let mut action = if resume_mid_run {
                sm.resume_after_preload()
            } else {
                sm.start(RuntimeTask::new(&goal).with_criteria(criteria))
            };

            while !sm.is_terminal() {
                next_archive_start = self
                    .append_observations(&session_id, &mut sm, next_archive_start)
                    .await;

                if self.interrupted.load(Ordering::Relaxed) {
                    let _ = sm.feed(LoopEvent::Timeout);
                    break;
                }

                if let Some(ss) = &self.opts.signal_source {
                    if let Some(sdk_sig) = ss.next_signal().await? {
                        let urgency = match sdk_sig.kind.as_str() {
                            "interrupt" => Urgency::Critical,
                            _ => Urgency::Normal,
                        };
                        let kernel_sig = KernelSignal::new(
                            KernelSignalSource::Custom,
                            KernelSignalType::Event,
                            urgency,
                            sdk_sig.kind.as_str(),
                        )
                        .with_payload(sdk_sig.payload.clone())
                        .with_timestamp(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        );
                        let executing = matches!(action, LoopAction::ExecuteTools { .. });
                        match router.ingest(kernel_sig, executing) {
                            SignalDisposition::InterruptNow | SignalDisposition::Interrupt => {
                                let _ = sm.feed(LoopEvent::Timeout);
                                break;
                            }
                            _ => {}
                        }
                    }
                }

                let mut queued = router.next();
                while let Some(sig) = queued {
                    if sig.urgency == Urgency::Critical {
                        let _ = sm.feed(LoopEvent::Timeout);
                        break;
                    }
                    queued = router.next();
                }
                if sm.is_terminal() {
                    break;
                }

                match &action {
                    LoopAction::CallLLM { context, tools } => {
                        let mut final_text = String::new();
                        let mut final_tool_calls: Vec<ToolCall> = Vec::new();
                        let mut turn_tokens: u32 = 0;

                        let mut provider_stream = match self
                            .opts
                            .provider
                            .stream(context, tools, ext.as_ref(), provider_state.as_ref())
                            .await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                yield RunEvent::Error(e.to_string());
                                let _ = sm.feed(LoopEvent::Timeout);
                                break;
                            }
                        };

                        while let Some(evt) = provider_stream.next().await {
                            match evt? {
                                StreamEvent::TextDelta { delta } => {
                                    final_text.push_str(&delta);
                                    yield RunEvent::TextDelta(delta);
                                }
                                StreamEvent::ThinkingDelta { delta } => {
                                    yield RunEvent::ThinkingDelta(delta);
                                }
                                StreamEvent::ToolCall { id, name, arguments } => {
                                    yield RunEvent::ToolCall { id: id.clone(), name: name.clone() };
                                    final_tool_calls.push(ToolCall {
                                        id: compact_str::CompactString::new(&id),
                                        name: compact_str::CompactString::new(&name),
                                        arguments,
                                    });
                                }
                                StreamEvent::Usage { total_tokens } => {
                                    turn_tokens = total_tokens;
                                }
                                StreamEvent::Done => {}
                            }
                        }

                        let assistant = Message {
                            role: deepstrike_core::types::message::Role::Assistant,
                            content: deepstrike_core::types::message::Content::Text(final_text.clone()),
                            tool_calls: final_tool_calls.clone(),
                            token_count: if turn_tokens > 0 { Some(turn_tokens) } else { None },
                        };

                        self.opts.provider.commit_stream_replay(&final_text, &final_tool_calls);
                        let provider_replay = peek_provider_replay(
                            self.opts.provider.as_ref(),
                            &final_text,
                            &final_tool_calls,
                        );

                        action = sm.feed(LoopEvent::LLMResponse { message: assistant.clone() });
                        self.log(
                            &session_id,
                            SessionEvent::LlmCompleted {
                                turn: sm.turn,
                                message: assistant,
                                provider_replay,
                            },
                        )
                        .await;
                    }
                    LoopAction::ExecuteTools { calls } => {
                        let tool_calls = calls.clone();
                        self.log(
                            &session_id,
                            SessionEvent::ToolRequested {
                                turn: sm.turn,
                                calls: tool_calls.clone(),
                            },
                        )
                        .await;

                        if let Some(gov) = &self.opts.governance {
                            let mut g = gov.lock().await;
                            if let Some(aid) = &self.opts.agent_id {
                                g.set_identity(aid, &session_id);
                            }
                        }

                        let run_ctx = RunContext {
                            agent_id: self.opts.agent_id.as_deref(),
                            skill_dir: self.opts.skill_dir.as_deref(),
                            dream_store: self.opts.dream_store.as_deref(),
                            knowledge_source: self.opts.knowledge_source.as_deref(),
                            governance: self.opts.governance.clone(),
                            on_tool_suspend: self.opts.on_tool_suspend.clone(),
                        };

                        let plane_stream = self.plane.execute_all(&tool_calls, run_ctx);
                        let mut tool_results = Vec::new();
                        let mut stream = plane_stream;
                        while let Some(evt) = stream.next().await {
                            match evt? {
                                RunEvent::ToolResult { call_id, content, is_error } => {
                                    tool_results.push(deepstrike_core::types::message::ToolResult {
                                        call_id: compact_str::CompactString::new(&call_id),
                                        output: deepstrike_core::types::message::Content::Text(content),
                                        is_error,
                                        token_count: None,
                                    });
                                }
                                other => yield other,
                            }
                        }

                        self.log(
                            &session_id,
                            SessionEvent::ToolCompleted {
                                turn: sm.turn,
                                results: tool_results.clone(),
                            },
                        )
                        .await;

                        action = sm.feed(LoopEvent::ToolResults { results: tool_results });
                    }
                    LoopAction::Done { result } => {
                        let status = format!("{:?}", result.termination).to_lowercase();
                        let turns_used = result.turns_used.max(1);
                        let total_tokens = result.total_tokens_used;

                        next_archive_start = self
                            .append_observations(&session_id, &mut sm, next_archive_start)
                            .await;

                        self.log(
                            &session_id,
                            SessionEvent::RunTerminal {
                                reason: status.clone(),
                                turns_used,
                                total_tokens,
                            },
                        )
                        .await;

                        if let (Some(store), Some(agent_id)) =
                            (&self.opts.dream_store, &self.opts.agent_id)
                        {
                            let new_msgs = sm.drain_new_messages();
                            if !new_msgs.is_empty() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;
                                let session = deepstrike_core::memory::durable::SessionData {
                                    session_id: uuid::Uuid::new_v4().to_string(),
                                    agent_id: agent_id.clone(),
                                    messages: new_msgs,
                                    metadata: serde_json::Value::Null,
                                    created_at_ms: session_start_ms,
                                    updated_at_ms: now_ms,
                                };
                                let _ = store.save_session(session).await;
                            }
                        }

                        yield RunEvent::Done {
                            iterations: turns_used,
                            total_tokens,
                            status,
                        };
                        return;
                    }
                }
            }

            next_archive_start = self
                .append_observations(&session_id, &mut sm, next_archive_start)
                .await;

            let reason = "error".to_string();
            self.log(
                &session_id,
                SessionEvent::RunTerminal {
                    reason: reason.clone(),
                    turns_used: sm.turn.max(1),
                    total_tokens: 0,
                },
            )
            .await;

            yield RunEvent::Done {
                iterations: sm.turn.max(1),
                total_tokens: 0,
                status: reason,
            };
        }
    }

    async fn append_observations(
        &self,
        session_id: &str,
        sm: &mut LoopStateMachine,
        mut next_archive_start: u64,
    ) -> u64 {
        let observations = sm.take_observations();
        for obs in observations {
            if !matches!(obs, LoopObservation::Compressed { .. }) {
                continue;
            }
            let Some(log) = &self.opts.session_log else {
                continue;
            };
            let latest = log.latest_seq(session_id).await.unwrap_or(-1) as u64;
            if latest < next_archive_start {
                continue;
            }
            let end = latest;
            if let Ok(compressed_seq) = log
                .append(
                    session_id,
                    SessionEvent::Compressed {
                        turn: sm.turn,
                        archived_seq_range: (next_archive_start, end),
                    },
                )
                .await
            {
                next_archive_start = compressed_seq + 1;
            }
        }
        next_archive_start
    }

    async fn read_entries(&self, session_id: &str) -> Result<Vec<SessionEntry>> {
        if let Some(log) = &self.opts.session_log {
            log.read(session_id, 0).await.map_err(Error::Io)
        } else {
            Ok(Vec::new())
        }
    }

    async fn log(&self, session_id: &str, event: SessionEvent) {
        if let Some(log) = &self.opts.session_log {
            let _ = log.append(session_id, event).await;
        }
    }
}

pub async fn collect_text(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + '_>>,
) -> Result<String> {
    let mut text = String::new();
    while let Some(evt) = stream.next().await {
        if let RunEvent::TextDelta(d) = evt? {
            text.push_str(&d);
        }
    }
    Ok(text)
}

fn merge_extensions(
    base: Option<&serde_json::Value>,
    over: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    match (base, over) {
        (Some(b), Some(o)) => {
            let mut merged = b.clone();
            if let (Some(m), Some(obj)) = (merged.as_object_mut(), o.as_object()) {
                for (k, v) in obj {
                    m.insert(k.clone(), v.clone());
                }
            }
            Some(merged)
        }
        (Some(b), None) => Some(b.clone()),
        (None, Some(o)) => Some(o.clone()),
        (None, None) => None,
    }
}

fn next_archived_seq_start(events: Option<&[SessionEntry]>) -> u64 {
    let mut next = 0u64;
    for entry in events.unwrap_or_default() {
        if let SessionEvent::Compressed {
            archived_seq_range,
            ..
        } = &entry.event
        {
            next = next.max(archived_seq_range.1 + 1);
        }
    }
    next
}

fn rendered_context_from_messages(
    messages: Vec<Message>,
) -> deepstrike_core::context::renderer::RenderedContext {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    for message in messages {
        if message.role == deepstrike_core::types::message::Role::System {
            if let Some(text) = message.content.as_text() {
                system_parts.push(text.to_owned());
            }
        } else {
            turns.push(message);
        }
    }
    deepstrike_core::context::renderer::RenderedContext {
        system_text: system_parts.join("\n\n"),
        turns,
    }
}

fn parse_frontmatter_description(content: &str) -> String {
    let body = content.trim_start();
    if !body.starts_with("---") {
        return String::new();
    }
    let rest = &body[3..];
    let end = rest.find("\n---").unwrap_or(rest.len());
    for line in rest[..end].lines() {
        if let Some(val) = line.strip_prefix("description:") {
            return val.trim().to_string();
        }
    }
    String::new()
}
