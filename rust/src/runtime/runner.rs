use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_stream::try_stream;
use deepstrike_core::memory::idle_pipeline::{IdleAction, IdleEvent, IdlePipeline, IdlePolicy};
use deepstrike_core::runtime::kernel::{
    KernelAction, KernelInput, KernelInputEvent, KernelObservation, KernelPressureAction,
    KernelRuntime, KernelStep,
};
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::types::milestone::MilestoneCheckResult;
use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::message::{Message, ToolCall};
use deepstrike_core::types::policy::SignalDisposition;
use deepstrike_core::types::signal::{
    RuntimeSignal as KernelSignal, SignalSource as KernelSignalSource,
    SignalType as KernelSignalType, Urgency,
};
use crate::runtime::sandboxed_skill::scan_skill_dir;
use crate::runtime::skill_watcher::SkillWatcher;
use deepstrike_core::types::task::RuntimeTask;
use futures::StreamExt;

use crate::SignalSource;
use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::{DreamResult, DreamStore};
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::archive::ArchiveStore;
use crate::runtime::execution_plane::{
    ExecutionPlane, LocalExecutionPlane, RunContext, ToolSuspendHandler,
};
use crate::runtime::provider_replay::{peek_provider_replay, seed_provider_replay_from_events};
use crate::runtime::replay::{
    is_mid_run, repair_entries_with_cap, replay_messages_with_cap,
    replay_messages_with_cap_and_loader,
};
use crate::runtime::session_log::{SessionEntry, SessionLog};
use crate::{Error, Result};
use deepstrike_core::context::task_state::TaskUpdate;
use deepstrike_core::runtime::repair::repair_llm_completed;

/// Controls what the runner does when the state machine returns
/// `EvaluateMilestone` — i.e., the LLM finished a turn but a milestone phase
/// has not yet been evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MilestonePolicy {
    /// Terminate the run immediately with `status = "milestone_pending"`.
    /// Callers that want real milestone evaluation must drive the SM themselves
    /// or use `AutoPass` for testing.  This is the **default**.
    #[default]
    Terminate,
    /// Unconditionally pass every milestone phase.  Useful in unit tests and
    /// capability-unlock–only scenarios where the criteria check is a no-op.
    AutoPass,
}

/// Configuration for a `RuntimeRunner` (aligned with Node/Python `RuntimeOptions`).
pub struct RuntimeOptions {
    pub provider: Box<dyn LLMProvider>,
    pub execution_plane: Option<Box<dyn ExecutionPlane>>,
    pub session_log: Option<Arc<dyn SessionLog>>,
    pub compression_store: Option<Arc<dyn ArchiveStore>>,
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
    pub tokenizer: Option<String>,
    pub enable_plan_tool: Option<bool>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
    /// How to handle `EvaluateMilestone` actions. Default: `Terminate`.
    pub milestone_policy: MilestonePolicy,
}

/// Orchestrates the agentic turn loop via the runtime kernel + session event log.
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
            _ => {
                return Err(Error::Other(
                    "unexpected IdlePipeline::Trigger action".into(),
                ));
            }
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

        let (curation_result, run_result) = match pipeline.feed(IdleEvent::SynthesisResult {
            content: synthesis_text,
        }) {
            IdleAction::CommitMemories {
                result, run_result, ..
            } => (result, run_result),
            _ => {
                return Err(Error::Other(
                    "unexpected IdlePipeline::SynthesisResult action".into(),
                ));
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

            let mut kernel = KernelRuntime::new(policy);
            let mut pending_observations = Vec::new();

            if let Some(tokenizer_name) = &self.opts.tokenizer {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetTokenizer {
                        name: tokenizer_name.clone(),
                    },
                );
            }
            if let Some(enabled) = self.opts.enable_plan_tool {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetPlanToolEnabled { enabled },
                );
            }

            kernel_apply(
                &mut kernel,
                &mut pending_observations,
                KernelInputEvent::SetTools {
                    tools: self.plane.schemas(),
                },
            );

            if self.opts.dream_store.is_some() && self.opts.agent_id.is_some() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetMemoryEnabled { enabled: true },
                );
            }
            if self.opts.knowledge_source.is_some() {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetKnowledgeEnabled { enabled: true },
                );
            }

            if let Some(sp) = &self.opts.system_prompt {
                let tokens = ((sp.len() / 4) as u32).max(1);
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::AddSystemMessage {
                        content: sp.clone(),
                        tokens,
                    },
                );
            }
            for mem in &self.opts.initial_memory {
                let tokens = ((mem.len() / 4) as u32).max(1);
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::AddMemoryMessage {
                        content: mem.clone(),
                        tokens,
                    },
                );
            }

            let skill_watcher = self.opts.skill_dir.as_deref().and_then(SkillWatcher::start);
            if let Some(skill_dir) = &self.opts.skill_dir {
                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::SetAvailableSkills {
                        skills: scan_skill_dir(skill_dir),
                    },
                );
            }

            let recovery_tokens = kernel
                .state_machine()
                .ctx
                .config
                .recovery_content_tokens(kernel.state_machine().ctx.max_tokens);
            let max_bytes = kernel
                .state_machine()
                .ctx
                .engine
                .token_budget_to_bytes(recovery_tokens);

            if let Some(ref events) = prior_events {
                let repaired = repair_entries_with_cap(events, max_bytes);
                seed_provider_replay_from_events(self.opts.provider.as_ref(), &repaired);

                let messages = if let Some(ref store) = self.opts.compression_store {
                    let store_clone = store.clone();
                    replay_messages_with_cap_and_loader(&repaired, max_bytes, move |archive_ref| {
                        store_clone.read(archive_ref).map_err(|_| {
                            deepstrike_core::context::snapshot::ContextFault::MissingArchive {
                                session_id: String::new(),
                                seq: 0,
                            }
                        })
                    })
                } else {
                    replay_messages_with_cap(&repaired, max_bytes)
                };

                kernel_apply(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::PreloadHistory {
                        messages,
                    },
                );
            }

            let ext = merge_extensions(self.opts.extensions.as_ref(), extensions.as_ref());
            let provider_state = self.opts.provider.create_run_state();
            let mut router = SignalRouter::new(256);
            let mut next_archive_start = next_archived_seq_start(prior_events.as_deref());
            let mut has_attempted_reactive_compact = false;
            let session_start_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let mut action = if resume_mid_run {
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::Resume,
                )
            } else {
                kernel_action(
                    &mut kernel,
                    &mut pending_observations,
                    KernelInputEvent::StartRun {
                        task: RuntimeTask::new(&goal).with_criteria(criteria),
                        run_spec: None,
                    },
                )
            };

            let mut last_skill_version: u64 = skill_watcher.as_ref().map(|w| w.version()).unwrap_or(0);

            while !kernel.is_terminal() {
                // Hot-reload: refresh skill catalog if the watcher detected changes.
                if let (Some(watcher), Some(skill_dir)) =
                    (&skill_watcher, &self.opts.skill_dir)
                {
                    let cur = watcher.version();
                    if cur != last_skill_version {
                        last_skill_version = cur;
                        kernel_apply(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::SetAvailableSkills {
                                skills: scan_skill_dir(skill_dir),
                            },
                        );
                    }
                }

                next_archive_start = self
                    .append_observations(
                        &session_id,
                        &kernel,
                        &mut pending_observations,
                        next_archive_start,
                    )
                    .await;

                if self.interrupted.load(Ordering::Relaxed) {
                    kernel_apply(
                        &mut kernel,
                        &mut pending_observations,
                        KernelInputEvent::Timeout,
                    );
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
                        let executing = matches!(action, KernelAction::ExecuteTool { .. });
                        match router.ingest(kernel_sig, executing) {
                            SignalDisposition::InterruptNow | SignalDisposition::Interrupt => {
                                kernel_apply(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::Timeout,
                                );
                                break;
                            }
                            _ => {}
                        }
                    }
                }

                let mut queued = router.next();
                while let Some(sig) = queued {
                    if sig.urgency == Urgency::Critical {
                        kernel_apply(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::Timeout,
                        );
                        break;
                    }
                    queued = router.next();
                }
                if kernel.is_terminal() {
                    break;
                }

                match &action {
                    KernelAction::CallProvider { context, tools } => {
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
                                if is_prompt_too_long_error(&e)
                                    && !has_attempted_reactive_compact
                                {
                                    has_attempted_reactive_compact = true;
                                    let compact_step = kernel.step(KernelInput::new(
                                        KernelInputEvent::ForceCompact,
                                    ));
                                    let compacted = compact_step.observations.iter().any(|obs| {
                                        matches!(obs, KernelObservation::Compressed { .. })
                                    });
                                    pending_observations.extend(compact_step.observations);
                                    if compacted {
                                        next_archive_start = self
                                            .append_observations(
                                                &session_id,
                                                &kernel,
                                                &mut pending_observations,
                                                next_archive_start,
                                            )
                                            .await;
                                        action = KernelAction::CallProvider {
                                            context: kernel.state_machine().ctx.render(),
                                            tools: tools.clone(),
                                        };
                                        continue;
                                    }
                                }
                                yield RunEvent::Error(e.to_string());
                                kernel_apply(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::Timeout,
                                );
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

                        let mut assistant = Message {
                            role: deepstrike_core::types::message::Role::Assistant,
                            content: deepstrike_core::types::message::Content::Text(final_text.clone()),
                            tool_calls: final_tool_calls.clone(),
                            token_count: if turn_tokens > 0 { Some(turn_tokens) } else { None },
                        };

                        self.opts.provider.commit_stream_replay(&final_text, &final_tool_calls);
                        let mut provider_replay = peek_provider_replay(
                            self.opts.provider.as_ref(),
                            &final_text,
                            &final_tool_calls,
                        );
                        repair_llm_completed(&mut assistant, &mut provider_replay);

                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ProviderResult {
                                message: assistant.clone(),
                            },
                        );
                        self.log(
                            &session_id,
                            SessionEvent::LlmCompleted {
                                turn: kernel.state_machine().turn,
                                message: assistant,
                                provider_replay,
                            },
                        )
                        .await;
                    }
                    KernelAction::ExecuteTool { calls } => {
                        let tool_calls = calls.clone();
                        self.log(
                            &session_id,
                            SessionEvent::ToolRequested {
                                turn: kernel.state_machine().turn,
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

                        let mut tool_results = Vec::new();
                        let mut normal_calls = Vec::new();
                        let mut plan_calls = Vec::new();

                        for call in &tool_calls {
                            if call.name == "update_plan" {
                                plan_calls.push(call);
                            } else {
                                normal_calls.push(call.clone());
                            }
                        }

                        for call in plan_calls {
                            let update = parse_update_plan_args(&call.arguments);
                            kernel_apply(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::UpdateTask { update },
                            );
                            tool_results.push(deepstrike_core::types::message::ToolResult {
                                call_id: call.id.clone(),
                                output: deepstrike_core::types::message::Content::Text("success".to_string()),
                                is_error: false,
                                is_fatal: false,
                                token_count: None,
                            });
                            yield RunEvent::ToolResult {
                                call_id: call.id.to_string(),
                                content: "success".to_string(),
                                is_error: false,
                            };
                        }

                        if !normal_calls.is_empty() {
                            let plane_stream = self.plane.execute_all(&normal_calls, run_ctx);
                            let mut stream = plane_stream;
                            while let Some(evt) = stream.next().await {
                                match evt? {
                                    RunEvent::ToolResult { call_id, content, is_error } => {
                                        tool_results.push(deepstrike_core::types::message::ToolResult {
                                            call_id: compact_str::CompactString::new(&call_id),
                                            output: deepstrike_core::types::message::Content::Text(content),
                                            is_error,
                                            is_fatal: false,
                                            token_count: None,
                                        });
                                    }
                                    RunEvent::ToolArgumentRepaired { call_id, name, original_arguments, repaired_arguments } => {
                                        self.log(
                                            &session_id,
                                            SessionEvent::ToolArgumentRepaired {
                                                turn: kernel.state_machine().turn,
                                                tool: name.clone(),
                                                original_arguments: original_arguments.clone(),
                                                repaired_arguments: repaired_arguments.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolArgumentRepaired {
                                            call_id,
                                            name,
                                            original_arguments,
                                            repaired_arguments,
                                        };
                                    }
                                    RunEvent::ToolDenied { call_id, tool_name, reason } => {
                                        self.log(
                                            &session_id,
                                            SessionEvent::ToolDenied {
                                                turn: kernel.state_machine().turn,
                                                call_id: call_id.clone(),
                                                tool_name: tool_name.clone(),
                                                reason: reason.clone(),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::ToolDenied { call_id, tool_name, reason };
                                    }
                                    RunEvent::PermissionRequest { call_id, tool_name, arguments, reason } => {
                                        let turn = kernel.state_machine().turn;
                                        self.log(
                                            &session_id,
                                            SessionEvent::PermissionRequested {
                                                turn,
                                                tool: tool_name.clone(),
                                                arguments: arguments.clone(),
                                                reason: Some(reason.clone()),
                                            },
                                        )
                                        .await;
                                        self.log(
                                            &session_id,
                                            SessionEvent::PermissionResolved {
                                                turn,
                                                approved: false,
                                                responder: "policy_gate".to_string(),
                                            },
                                        )
                                        .await;
                                        self.log(
                                            &session_id,
                                            SessionEvent::ToolDenied {
                                                turn,
                                                call_id: call_id.clone(),
                                                tool_name: tool_name.clone(),
                                                reason: format!("permission denied by policy gate: {reason}"),
                                            },
                                        )
                                        .await;
                                        yield RunEvent::PermissionRequest { call_id, tool_name, arguments, reason };
                                    }
                                    other => yield other,
                                }
                            }
                            let names: Vec<String> = normal_calls.iter().map(|c| c.name.to_string()).collect();
                            kernel_apply(
                                &mut kernel,
                                &mut pending_observations,
                                KernelInputEvent::UpdateTask {
                                    update: TaskUpdate {
                                        progress: Some(format!("Executed tools: {}", names.join(", "))),
                                        ..Default::default()
                                    },
                                },
                            );
                        }

                        self.log(
                            &session_id,
                            SessionEvent::ToolCompleted {
                                turn: kernel.state_machine().turn,
                                results: tool_results.clone(),
                            },
                        )
                        .await;

                        action = kernel_action(
                            &mut kernel,
                            &mut pending_observations,
                            KernelInputEvent::ToolResults {
                                results: tool_results,
                            },
                        );
                    }
                    KernelAction::EvaluateMilestone { phase_id, criteria: _ } => {
                        match self.opts.milestone_policy {
                            MilestonePolicy::AutoPass => {
                                let result = MilestoneCheckResult::pass(phase_id.clone());
                                action = kernel_action(
                                    &mut kernel,
                                    &mut pending_observations,
                                    KernelInputEvent::MilestoneResult { result },
                                );
                                next_archive_start = self
                                    .append_observations(
                                        &session_id,
                                        &kernel,
                                        &mut pending_observations,
                                        next_archive_start,
                                    )
                                    .await;
                            }
                            MilestonePolicy::Terminate => {
                                // No external verifier — terminate so the caller can drive
                                // evaluation themselves via a custom run loop.
                                next_archive_start = self
                                    .append_observations(
                                        &session_id,
                                        &kernel,
                                        &mut pending_observations,
                                        next_archive_start,
                                    )
                                    .await;
                                self.log(
                                    &session_id,
                                    SessionEvent::RunTerminal {
                                        reason: "milestone_pending".to_string(),
                                        turns_used: kernel.state_machine().turn.max(1),
                                        total_tokens: 0,
                                    },
                                )
                                .await;
                                yield RunEvent::Done {
                                    iterations: kernel.state_machine().turn.max(1),
                                    total_tokens: 0,
                                    status: "milestone_pending".to_string(),
                                };
                                return;
                            }
                        }
                    }
                    KernelAction::Done { result } => {
                        let status = format!("{:?}", result.termination).to_lowercase();
                        let turns_used = result.turns_used.max(1);
                        let total_tokens = result.total_tokens_used;

                        next_archive_start = self
                            .append_observations(
                                &session_id,
                                &kernel,
                                &mut pending_observations,
                                next_archive_start,
                            )
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
                            let new_msgs = kernel.state_machine_mut().drain_new_messages();
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
                .append_observations(
                    &session_id,
                    &kernel,
                    &mut pending_observations,
                    next_archive_start,
                )
                .await;

            let reason = "error".to_string();
            self.log(
                &session_id,
                SessionEvent::RunTerminal {
                    reason: reason.clone(),
                    turns_used: kernel.state_machine().turn.max(1),
                    total_tokens: 0,
                },
            )
            .await;

            yield RunEvent::Done {
                iterations: kernel.state_machine().turn.max(1),
                total_tokens: 0,
                status: reason,
            };
        }
    }

    async fn append_observations(
        &self,
        session_id: &str,
        kernel: &KernelRuntime,
        observations: &mut Vec<KernelObservation>,
        mut next_archive_start: u64,
    ) -> u64 {
        let sm = kernel.state_machine();
        let turn = sm.turn;
        let preserved_refs = sm.ctx.partitions.task_state.preserved_refs.clone();
        let drained = std::mem::take(observations);
        for obs in drained {
            match obs {
                KernelObservation::Compressed {
                    action,
                    rho_after: _,
                    summary,
                    archived,
                } => {
                    let Some(log) = &self.opts.session_log else {
                        continue;
                    };
                    let latest = log.latest_seq(session_id).await.unwrap_or(-1) as u64;
                    if latest < next_archive_start {
                        continue;
                    }
                    let end = latest;

                    let mut archive_ref = None;
                    if let Some(store) = &self.opts.compression_store {
                        if !archived.is_empty() {
                            if let Ok(path_ref) = store.write(session_id, next_archive_start, &archived) {
                                if !path_ref.is_empty() {
                                    archive_ref = Some(path_ref);
                                }
                            }
                        }
                    }

                    let summary_tokens = summary.as_ref().map(|s| sm.ctx.engine.count(s));
                    let action_str = match action {
                        KernelPressureAction::None => "none".to_string(),
                        KernelPressureAction::SnipCompact => "snip_compact".to_string(),
                        KernelPressureAction::MicroCompact => "micro_compact".to_string(),
                        KernelPressureAction::ContextCollapse => "context_collapse".to_string(),
                        KernelPressureAction::AutoCompact => "auto_compact".to_string(),
                    };

                    if let Ok(compressed_seq) = log
                        .append(
                            session_id,
                            SessionEvent::Compressed {
                                turn,
                                archived_seq_range: (next_archive_start, end),
                                action: Some(action_str),
                                summary: summary.clone(),
                                summary_tokens,
                                archive_ref,
                                preserved_refs: preserved_refs.clone(),
                            },
                        )
                        .await
                    {
                        next_archive_start = compressed_seq + 1;
                    }
                }
                KernelObservation::Rollbacked {
                    turn,
                    checkpoint_history_len,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::Rollbacked {
                            turn,
                            checkpoint_history_len,
                        },
                    )
                    .await;
                }
                KernelObservation::CapabilityChanged {
                    turn,
                    added,
                    removed,
                    change_kind,
                    capability_id,
                    version,
                    mounted_by,
                    mount_reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::CapabilityChanged {
                            turn,
                            added,
                            removed,
                            change_kind,
                            capability_id,
                            version,
                            mounted_by,
                            mount_reason,
                        },
                    )
                    .await;
                }
                KernelObservation::MilestoneAdvanced {
                    turn,
                    phase_id,
                    capabilities_unlocked,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MilestoneAdvanced {
                            turn,
                            phase_id,
                            capabilities_unlocked,
                        },
                    )
                    .await;
                }
                KernelObservation::MilestoneBlocked {
                    turn,
                    phase_id,
                    reason,
                } => {
                    self.log(
                        session_id,
                        SessionEvent::MilestoneBlocked {
                            turn,
                            phase_id,
                            reason,
                        },
                    )
                    .await;
                }
                KernelObservation::Renewed { .. } => {}
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

fn kernel_apply(
    kernel: &mut KernelRuntime,
    pending_observations: &mut Vec<KernelObservation>,
    event: KernelInputEvent,
) {
    let step = kernel.step(KernelInput::new(event));
    pending_observations.extend(step.observations);
}

fn kernel_action(
    kernel: &mut KernelRuntime,
    pending_observations: &mut Vec<KernelObservation>,
    event: KernelInputEvent,
) -> KernelAction {
    let mut step = kernel.step(KernelInput::new(event));
    pending_observations.append(&mut step.observations);
    take_single_action(step)
}

fn take_single_action(mut step: KernelStep) -> KernelAction {
    step.actions
        .pop()
        .expect("kernel transition must return one action")
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

fn is_prompt_too_long_error(error: &Error) -> bool {
    let msg = error.to_string().to_lowercase();
    msg.contains("413")
        || msg.contains("too long")
        || msg.contains("prompt too long")
        || msg.contains("context length exceeded")
        || msg.contains("context_length_exceeded")
}

fn next_archived_seq_start(events: Option<&[SessionEntry]>) -> u64 {
    let mut next = 0u64;
    for entry in events.unwrap_or_default() {
        if let SessionEvent::Compressed {
            archived_seq_range, ..
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

fn parse_update_plan_args(val: &serde_json::Value) -> TaskUpdate {
    let plan = val.get("plan").and_then(|v| {
        v.as_array().map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
    });
    let current_step = val
        .get("current_step")
        .or_else(|| val.get("currentStep"))
        .and_then(|v| v.as_u64().map(|x| x as usize));
    let progress = val
        .get("progress")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let scratchpad = val
        .get("scratchpad")
        .and_then(|v| v.as_str().map(|s| s.to_string()));
    let blocked_on = val
        .get("blocked_on")
        .or_else(|| val.get("blockedOn"))
        .and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });
    let preserved_refs = val
        .get("preserved_refs")
        .or_else(|| val.get("preservedRefs"))
        .and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
        });
    TaskUpdate {
        plan,
        current_step,
        progress,
        scratchpad,
        blocked_on,
        preserved_refs,
    }
}
