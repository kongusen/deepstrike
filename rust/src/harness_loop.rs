//! Policy-oriented attempt orchestration.
//!
//! The four independent axes are explicit: [`AttemptBody`] runs work,
//! [`AttemptJudge`] evaluates it, [`CarryPolicy`] prepares the next attempt, and
//! [`StopPolicy`] bounds the loop.  Runtime health and quality verdict are kept
//! as separate fields in [`AttemptOutcome`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::harness::eval::SkillCandidate;
use deepstrike_core::orchestration::workflow::WorkflowNode;
use deepstrike_core::types::message::{Message, Role};
use futures::{Stream, StreamExt};

use crate::harness::{Criterion, Verdict};
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::RuntimeRunner;
use crate::tools::ToolChunk;
use crate::{Error, Result};

pub type AttemptBodyStream<'a> = Pin<Box<dyn Stream<Item = Result<AttemptBodyEvent>> + 'a>>;
pub type AttemptLoopStream<'a> = Pin<Box<dyn Stream<Item = Result<AttemptLoopEvent>> + 'a>>;

#[derive(Debug, Clone)]
pub struct AttemptRequest {
    pub session_id: String,
    pub goal: String,
    pub criteria: Vec<Criterion>,
    pub extensions: Option<serde_json::Value>,
}

impl AttemptRequest {
    pub fn new(session_id: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            goal: goal.into(),
            criteria: Vec::new(),
            extensions: None,
        }
    }

    pub fn generated(goal: impl Into<String>) -> Self {
        Self::new(uuid::Uuid::new_v4().to_string(), goal)
    }
}

#[derive(Debug, Clone)]
pub struct AttemptBodyContext {
    pub session_id: String,
    pub goal: String,
    pub criteria: Vec<Criterion>,
    pub extensions: Option<serde_json::Value>,
    pub attempt: u32,
    /// Carry material is context for the next attempt, not a goal rewrite.
    pub context_input: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AttemptBodyEvent {
    Token(String),
    ToolCall {
        id: String,
        name: String,
    },
    ToolDelta {
        call_id: String,
        name: String,
        chunk: ToolChunk,
    },
    ToolSuspend {
        call_id: String,
        name: String,
        suspension_id: String,
        payload: Option<serde_json::Value>,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    WorkflowNodesSubmitted(Vec<WorkflowNode>),
    BodyError(String),
    BodyDone {
        run_status: String,
        result: String,
        turns: u32,
        total_tokens: u64,
    },
}

pub trait AttemptBody: Send + Sync {
    fn run<'a>(&'a self, context: AttemptBodyContext) -> AttemptBodyStream<'a>;
}

/// Adapts [`RuntimeRunner`] to the body policy slot.
pub struct RuntimeAttemptBody<'a> {
    runner: &'a RuntimeRunner,
}

impl<'a> RuntimeAttemptBody<'a> {
    pub fn new(runner: &'a RuntimeRunner) -> Self {
        Self { runner }
    }
}

impl AttemptBody for RuntimeAttemptBody<'_> {
    fn run<'a>(&'a self, context: AttemptBodyContext) -> AttemptBodyStream<'a> {
        Box::pin(async_stream::try_stream! {
            let mut criteria: Vec<String> = context
                .criteria
                .iter()
                .map(|criterion| criterion.text.clone())
                .collect();
            // RuntimeRunner has no mutable side-channel. A carried note is therefore supplied as
            // run context while the stable session id preserves the transcript. The goal itself
            // remains byte-for-byte stable under ContinueSession.
            if let Some(note) = &context.context_input {
                criteria.push(format!("[Attempt feedback] {note}"));
            }

            let mut result = String::new();
            let mut terminal: Option<(u32, u64, String)> = None;
            let stream = self.runner
                .run_streaming(
                    &context.goal,
                    &criteria,
                    context.extensions.as_ref(),
                    Some(&context.session_id),
                )
                .await;
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(error) => {
                    yield AttemptBodyEvent::BodyError(error.to_string());
                    yield AttemptBodyEvent::BodyDone {
                        run_status: "error".to_string(),
                        result,
                        turns: 0,
                        total_tokens: 0,
                    };
                    return;
                }
            };

            while let Some(event) = stream.next().await {
                match event {
                    Err(error) => {
                        yield AttemptBodyEvent::BodyError(error.to_string());
                        terminal = Some((0, 0, "error".to_string()));
                        break;
                    }
                    Ok(RunEvent::TextDelta(delta)) => {
                        result.push_str(&delta);
                        yield AttemptBodyEvent::Token(delta);
                    }
                    Ok(RunEvent::ToolCall { id, name }) => {
                        yield AttemptBodyEvent::ToolCall { id, name };
                    }
                    Ok(RunEvent::ToolDelta { call_id, name, chunk }) => {
                        yield AttemptBodyEvent::ToolDelta { call_id, name, chunk };
                    }
                    Ok(RunEvent::ToolSuspend { call_id, name, suspension_id, payload }) => {
                        yield AttemptBodyEvent::ToolSuspend {
                            call_id,
                            name,
                            suspension_id,
                            payload,
                        };
                    }
                    Ok(RunEvent::ToolResult { call_id, content, is_error, .. }) => {
                        yield AttemptBodyEvent::ToolResult { call_id, content, is_error };
                    }
                    Ok(RunEvent::Error(message)) => {
                        yield AttemptBodyEvent::BodyError(message);
                        terminal = Some((0, 0, "error".to_string()));
                        break;
                    }
                    Ok(RunEvent::Done { iterations, total_tokens, status }) => {
                        terminal = Some((iterations, total_tokens, status));
                    }
                    Ok(_) => {}
                }
            }

            let (turns, total_tokens, run_status) =
                terminal.unwrap_or_else(|| (0, 0, "error".to_string()));
            yield AttemptBodyEvent::BodyDone {
                run_status,
                result,
                turns,
                total_tokens,
            };
        })
    }
}

#[derive(Debug, Clone)]
pub struct JudgeContext {
    pub goal: String,
    pub criteria: Vec<Criterion>,
    pub attempt: u32,
    pub result: String,
}

#[derive(Debug, Clone)]
pub struct JudgeResult {
    pub verdict: Verdict,
    pub skill_candidate: Option<SkillCandidate>,
}

impl JudgeResult {
    pub fn new(verdict: Verdict) -> Self {
        Self {
            verdict,
            skill_candidate: None,
        }
    }
}

#[async_trait]
pub trait AttemptJudge: Send + Sync {
    /// `None` defers to a composed fallback judge.
    async fn judge(&self, context: &JudgeContext) -> Result<Option<JudgeResult>>;
}

pub type VerdictFn = Arc<dyn Fn(&JudgeContext) -> Option<Verdict> + Send + Sync>;

pub struct VerdictFnJudge {
    verdict_fn: VerdictFn,
}

impl VerdictFnJudge {
    pub fn new(verdict_fn: VerdictFn) -> Self {
        Self { verdict_fn }
    }
}

#[async_trait]
impl AttemptJudge for VerdictFnJudge {
    async fn judge(&self, context: &JudgeContext) -> Result<Option<JudgeResult>> {
        Ok((self.verdict_fn)(context).map(JudgeResult::new))
    }
}

pub struct LlmEvalJudge {
    eval_provider: Box<dyn LLMProvider>,
    extract_skill_on_pass: bool,
}

impl LlmEvalJudge {
    pub fn new(eval_provider: impl LLMProvider + 'static) -> Self {
        Self {
            eval_provider: Box::new(eval_provider),
            extract_skill_on_pass: false,
        }
    }

    pub fn extract_skill_on_pass(mut self, enabled: bool) -> Self {
        self.extract_skill_on_pass = enabled;
        self
    }
}

#[async_trait]
impl AttemptJudge for LlmEvalJudge {
    async fn judge(&self, context: &JudgeContext) -> Result<Option<JudgeResult>> {
        use deepstrike_core::harness::eval::{
            build_eval_messages, parse_verdict, Criterion as EvalCriterion,
        };

        let criteria: Vec<EvalCriterion> = context
            .criteria
            .iter()
            .map(|criterion| EvalCriterion {
                text: criterion.text.clone(),
                required: criterion.required,
                weight: criterion.weight,
            })
            .collect();
        let messages = build_eval_messages(
            &context.goal,
            &criteria,
            &context.result,
            context.attempt,
            self.extract_skill_on_pass,
        );
        let rendered = rendered_context_from_messages(messages);
        let state = self.eval_provider.create_run_state();
        let mut stream = self
            .eval_provider
            .stream(&rendered, &[], None, state.as_ref())
            .await?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta { delta } = event? {
                text.push_str(&delta);
            }
        }
        if text.is_empty() {
            return Err(Error::Other("attempt judge produced no text".to_string()));
        }

        let parsed = parse_verdict(&text);
        Ok(Some(JudgeResult {
            verdict: Verdict {
                passed: parsed.passed,
                overall_score: parsed.overall_score,
                feedback: parsed.feedback,
                details: parsed
                    .details
                    .into_iter()
                    .map(|detail| crate::harness::CriterionResult {
                        criterion: detail.criterion,
                        passed: detail.passed,
                        score: detail.score,
                        feedback: detail.feedback,
                    })
                    .collect(),
            },
            skill_candidate: parsed.skill_candidate,
        }))
    }
}

pub struct HybridJudge<P, F> {
    primary: P,
    fallback: F,
}

impl<P, F> HybridJudge<P, F> {
    pub fn new(primary: P, fallback: F) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl<P, F> AttemptJudge for HybridJudge<P, F>
where
    P: AttemptJudge,
    F: AttemptJudge,
{
    async fn judge(&self, context: &JudgeContext) -> Result<Option<JudgeResult>> {
        match self.primary.judge(context).await? {
            Some(result) => Ok(Some(result)),
            None => self.fallback.judge(context).await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CarryContext {
    pub root_session_id: String,
    pub goal: String,
    pub attempt: u32,
    pub previous_verdict: Option<Verdict>,
}

#[derive(Debug, Clone)]
pub struct PreparedAttempt {
    pub session_id: String,
    pub goal: String,
    pub context_input: Option<String>,
}

#[async_trait]
pub trait CarryPolicy: Send + Sync {
    async fn prepare(&self, context: CarryContext) -> Result<PreparedAttempt>;
}

/// Default carry: stable session + unchanged goal + feedback as context.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContinueSession;

#[async_trait]
impl CarryPolicy for ContinueSession {
    async fn prepare(&self, context: CarryContext) -> Result<PreparedAttempt> {
        Ok(PreparedAttempt {
            session_id: context.root_session_id,
            goal: context.goal,
            context_input: context
                .previous_verdict
                .map(|verdict| verdict.feedback)
                .filter(|feedback| !feedback.is_empty()),
        })
    }
}

/// Explicit isolation: a new session and goal-appended feedback after attempt 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct FreshWithFeedback;

#[async_trait]
impl CarryPolicy for FreshWithFeedback {
    async fn prepare(&self, context: CarryContext) -> Result<PreparedAttempt> {
        let goal = match context.previous_verdict {
            Some(verdict) if !verdict.feedback.is_empty() => format!(
                "{}\n\n[Attempt {} feedback: {}]",
                context.goal,
                context.attempt.saturating_sub(1),
                verdict.feedback
            ),
            _ => context.goal,
        };
        Ok(PreparedAttempt {
            session_id: if context.attempt == 1 {
                context.root_session_id
            } else {
                uuid::Uuid::new_v4().to_string()
            },
            goal,
            context_input: None,
        })
    }
}

pub type DigestFuture = Pin<Box<dyn Future<Output = Result<String>> + Send>>;
pub type DigestFn = Arc<dyn Fn(Verdict, u32) -> DigestFuture + Send + Sync>;
pub type PassHookFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub type PassHook =
    Arc<dyn for<'a> Fn(&'a AttemptOutcome, &'a JudgeResult) -> PassHookFuture<'a> + Send + Sync>;

pub struct FreshWithDigest {
    digest: DigestFn,
}

impl FreshWithDigest {
    pub fn new(digest: DigestFn) -> Self {
        Self { digest }
    }
}

#[async_trait]
impl CarryPolicy for FreshWithDigest {
    async fn prepare(&self, context: CarryContext) -> Result<PreparedAttempt> {
        let goal = if let Some(verdict) = context.previous_verdict {
            let digest = (self.digest)(verdict, context.attempt.saturating_sub(1)).await?;
            format!("{}\n\n[Prior attempt digest: {digest}]", context.goal)
        } else {
            context.goal
        };
        Ok(PreparedAttempt {
            session_id: if context.attempt == 1 {
                context.root_session_id
            } else {
                uuid::Uuid::new_v4().to_string()
            },
            goal,
            context_input: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct StopPolicy {
    pub max_attempts: u32,
    pub max_total_tokens: Option<u64>,
    pub stop_on_failed_verdict: bool,
}

impl StopPolicy {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            max_total_tokens: None,
            stop_on_failed_verdict: false,
        }
    }

    pub fn max_total_tokens(mut self, limit: u64) -> Self {
        self.max_total_tokens = Some(limit);
        self
    }

    pub fn stop_on_failed_verdict(mut self, enabled: bool) -> Self {
        self.stop_on_failed_verdict = enabled;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcomeKind {
    Passed,
    FailedJudge,
    Exhausted,
    RunError,
}

#[derive(Debug, Clone)]
pub struct AttemptOutcome {
    pub outcome: AttemptOutcomeKind,
    pub run_status: String,
    pub verdict: Option<Verdict>,
    pub result: String,
    pub attempts: u32,
    pub turns: u32,
    pub total_tokens: u64,
    pub submitted_nodes: Option<Vec<WorkflowNode>>,
}

#[derive(Debug, Clone)]
pub enum AttemptLoopEvent {
    Token(String),
    ToolCall {
        id: String,
        name: String,
    },
    ToolDelta {
        call_id: String,
        name: String,
        chunk: ToolChunk,
    },
    ToolSuspend {
        call_id: String,
        name: String,
        suspension_id: String,
        payload: Option<serde_json::Value>,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    WorkflowNodesSubmitted(Vec<WorkflowNode>),
    BodyError(String),
    Judging {
        attempt: u32,
    },
    Retrying {
        attempt: u32,
        verdict: Verdict,
    },
    Completed(AttemptOutcome),
}

pub struct AttemptLoop<B, J, C = ContinueSession> {
    body: B,
    judge: J,
    carry: C,
    stop: StopPolicy,
    on_pass: Option<PassHook>,
}

impl<B, J> AttemptLoop<B, J, ContinueSession> {
    pub fn new(body: B, judge: J, stop: StopPolicy) -> Result<Self> {
        validate_stop_policy(&stop)?;
        Ok(Self {
            body,
            judge,
            carry: ContinueSession,
            stop,
            on_pass: None,
        })
    }
}

impl<B, J, C> AttemptLoop<B, J, C> {
    pub fn with_carry<Next>(self, carry: Next) -> AttemptLoop<B, J, Next> {
        AttemptLoop {
            body: self.body,
            judge: self.judge,
            carry,
            stop: self.stop,
            on_pass: self.on_pass,
        }
    }

    /// Runs after a passing verdict and before the completed event is emitted.
    pub fn with_on_pass(mut self, hook: PassHook) -> Self {
        self.on_pass = Some(hook);
        self
    }
}

impl<B, J, C> AttemptLoop<B, J, C>
where
    B: AttemptBody,
    J: AttemptJudge,
    C: CarryPolicy,
{
    pub async fn run(&self, request: AttemptRequest) -> Result<AttemptOutcome> {
        let mut outcome = None;
        let mut stream = self.stream(request);
        while let Some(event) = stream.next().await {
            if let AttemptLoopEvent::Completed(completed) = event? {
                outcome = Some(completed);
            }
        }
        outcome.ok_or_else(|| Error::Other("AttemptLoop ended without an outcome".to_string()))
    }

    pub fn stream<'a>(&'a self, request: AttemptRequest) -> AttemptLoopStream<'a> {
        Box::pin(async_stream::try_stream! {
            let root_session_id = request.session_id.clone();
            let mut previous_verdict: Option<Verdict> = None;
            let mut total_tokens = 0u64;
            let mut total_turns = 0u32;
            let mut submitted_nodes = Vec::new();

            for attempt in 1..=self.stop.max_attempts {
                let prepared = self.carry
                    .prepare(CarryContext {
                        root_session_id: root_session_id.clone(),
                        goal: request.goal.clone(),
                        attempt,
                        previous_verdict: previous_verdict.clone(),
                    })
                    .await?;
                let mut body = self.body.run(AttemptBodyContext {
                    session_id: prepared.session_id,
                    goal: prepared.goal,
                    criteria: request.criteria.clone(),
                    extensions: request.extensions.clone(),
                    attempt,
                    context_input: prepared.context_input,
                });
                let mut terminal = None;
                while let Some(event) = body.next().await {
                    match event? {
                        AttemptBodyEvent::Token(text) => yield AttemptLoopEvent::Token(text),
                        AttemptBodyEvent::ToolCall { id, name } => {
                            yield AttemptLoopEvent::ToolCall { id, name };
                        }
                        AttemptBodyEvent::ToolDelta { call_id, name, chunk } => {
                            yield AttemptLoopEvent::ToolDelta { call_id, name, chunk };
                        }
                        AttemptBodyEvent::ToolSuspend {
                            call_id,
                            name,
                            suspension_id,
                            payload,
                        } => {
                            yield AttemptLoopEvent::ToolSuspend {
                                call_id,
                                name,
                                suspension_id,
                                payload,
                            };
                        }
                        AttemptBodyEvent::ToolResult { call_id, content, is_error } => {
                            yield AttemptLoopEvent::ToolResult { call_id, content, is_error };
                        }
                        AttemptBodyEvent::WorkflowNodesSubmitted(nodes) => {
                            submitted_nodes.extend(nodes.iter().cloned());
                            yield AttemptLoopEvent::WorkflowNodesSubmitted(nodes);
                        }
                        AttemptBodyEvent::BodyError(message) => {
                            yield AttemptLoopEvent::BodyError(message);
                        }
                        AttemptBodyEvent::BodyDone {
                            run_status,
                            result,
                            turns,
                            total_tokens,
                        } => terminal = Some((run_status, result, turns, total_tokens)),
                    }
                }
                let (run_status, result, turns, attempt_tokens) = terminal.ok_or_else(|| {
                    Error::Other("AttemptBody ended without body_done".to_string())
                })?;
                total_tokens = total_tokens.saturating_add(attempt_tokens);
                total_turns = total_turns.saturating_add(turns);

                if is_run_error(&run_status) {
                    yield AttemptLoopEvent::Completed(AttemptOutcome {
                        outcome: AttemptOutcomeKind::RunError,
                        run_status,
                        verdict: None,
                        result,
                        attempts: attempt,
                        turns: total_turns,
                        total_tokens,
                        submitted_nodes: (!submitted_nodes.is_empty())
                            .then(|| submitted_nodes.clone()),
                    });
                    return;
                }

                yield AttemptLoopEvent::Judging { attempt };
                let judged = self
                    .judge
                    .judge(&JudgeContext {
                        goal: request.goal.clone(),
                        criteria: request.criteria.clone(),
                        attempt,
                        result: result.clone(),
                    })
                    .await?
                    .ok_or_else(|| Error::Other("AttemptLoop judge produced no verdict".to_string()))?;
                let verdict = judged.verdict.clone();

                if verdict.passed {
                    let outcome = AttemptOutcome {
                        outcome: AttemptOutcomeKind::Passed,
                        run_status,
                        verdict: Some(verdict),
                        result,
                        attempts: attempt,
                        turns: total_turns,
                        total_tokens,
                        submitted_nodes: (!submitted_nodes.is_empty())
                            .then(|| submitted_nodes.clone()),
                    };
                    if let Some(on_pass) = &self.on_pass {
                        on_pass(&outcome, &judged).await?;
                    }
                    yield AttemptLoopEvent::Completed(outcome);
                    return;
                }

                previous_verdict = Some(verdict.clone());
                let token_limit_reached = self
                    .stop
                    .max_total_tokens
                    .is_some_and(|limit| total_tokens >= limit);
                if self.stop.stop_on_failed_verdict
                    || attempt == self.stop.max_attempts
                    || token_limit_reached
                {
                    yield AttemptLoopEvent::Completed(AttemptOutcome {
                        outcome: if self.stop.stop_on_failed_verdict {
                            AttemptOutcomeKind::FailedJudge
                        } else {
                            AttemptOutcomeKind::Exhausted
                        },
                        run_status,
                        verdict: Some(verdict),
                        result,
                        attempts: attempt,
                        turns: total_turns,
                        total_tokens,
                        submitted_nodes: (!submitted_nodes.is_empty())
                            .then(|| submitted_nodes.clone()),
                    });
                    return;
                }

                yield AttemptLoopEvent::Retrying { attempt, verdict };
            }
        })
    }
}

fn validate_stop_policy(stop: &StopPolicy) -> Result<()> {
    if stop.max_attempts == 0 {
        return Err(Error::Other(
            "AttemptLoop stop.max_attempts must be positive".to_string(),
        ));
    }
    Ok(())
}

fn is_run_error(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "error" | "invalid_arg" | "user_abort"
    )
}

fn rendered_context_from_messages(messages: Vec<Message>) -> RenderedContext {
    let mut system_parts = Vec::new();
    let mut turns = Vec::new();
    for message in messages {
        if message.role == Role::System {
            if let Some(text) = message.content.as_text() {
                system_parts.push(text.to_owned());
            }
        } else {
            turns.push(message);
        }
    }
    let system_text = system_parts.join("\n\n");
    RenderedContext {
        system_text: system_text.clone(),
        system_stable: system_text,
        system_knowledge: String::new(),
        turns,
        state_turn: None,
        frozen_prefix_len: None,
        budget_overflow: None,
    }
}
