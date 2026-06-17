use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::types::message::{Message, Role};
use futures::StreamExt;

use crate::Result;
use crate::harness::{HarnessEvent, HarnessOutcome, HarnessRequest, QualityGate, Verdict};
use crate::providers::{LLMProvider, StreamEvent};
use crate::run_event::RunEvent;
use crate::runtime::RuntimeRunner;

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
    }
}

async fn collect_run_runtime(
    runner: &RuntimeRunner,
    req: &HarnessRequest,
) -> Result<(String, u32, u64, String)> {
    let criteria_texts: Vec<String> = req.criteria.iter().map(|c| c.text.clone()).collect();
    let mut text = String::new();
    let mut iterations = 0u32;
    let mut total_tokens = 0u64;
    let mut status = "error".to_string();
    let mut stream = runner
        .run_streaming(&req.goal, &criteria_texts, req.extensions.as_ref(), None)
        .await?;
    while let Some(evt) = stream.next().await {
        match evt? {
            RunEvent::TextDelta(d) => text.push_str(&d),
            RunEvent::Done {
                iterations: i,
                total_tokens: t,
                status: s,
            } => {
                iterations = i;
                total_tokens = t;
                status = s;
            }
            _ => {}
        }
    }
    Ok((text, iterations, total_tokens, status))
}

/// SinglePassHarness — run once, always passes.
pub struct SinglePassHarness<'a> {
    runner: &'a RuntimeRunner,
}

impl<'a> SinglePassHarness<'a> {
    pub fn new(runner: &'a RuntimeRunner) -> Self {
        Self { runner }
    }

    pub async fn run(&self, request: HarnessRequest) -> Result<HarnessOutcome> {
        let (text, iterations, total_tokens, status) =
            collect_run_runtime(self.runner, &request).await?;
        Ok(HarnessOutcome {
            result: text,
            passed: true,
            iterations,
            total_tokens,
            status,
            overall_score: 1.0,
            feedback: None,
            details: vec![],
        })
    }
}

/// EvalLoopHarness — retry until QualityGate passes (deprecated, use HarnessLoop).
pub struct EvalLoopHarness<'a, G: QualityGate> {
    runner: &'a RuntimeRunner,
    gate: G,
    max_attempts: usize,
}

impl<'a, G: QualityGate> EvalLoopHarness<'a, G> {
    pub fn new(runner: &'a RuntimeRunner, gate: G, max_attempts: usize) -> Self {
        Self {
            runner,
            gate,
            max_attempts,
        }
    }

    pub async fn run(&self, request: HarnessRequest) -> Result<HarnessOutcome> {
        let mut outcome = HarnessOutcome {
            result: String::new(),
            passed: false,
            iterations: 0,
            total_tokens: 0,
            status: "error".into(),
            overall_score: 0.0,
            feedback: None,
            details: vec![],
        };
        for _ in 0..self.max_attempts {
            let (text, iterations, total_tokens, status) =
                collect_run_runtime(self.runner, &request).await?;
            outcome = HarnessOutcome {
                result: text,
                passed: false,
                iterations,
                total_tokens,
                status,
                overall_score: 0.0,
                feedback: None,
                details: vec![],
            };
            if self.gate.evaluate(&request, &outcome).await? {
                outcome.passed = true;
                return Ok(outcome);
            }
        }
        Ok(outcome)
    }
}

/// I3.2 (A2/A3): host-supplied judgment for each attempt's result. Mirrors the Node SDK
/// `VerdictFn`. Returning `Some(Verdict)` short-circuits the built-in LLM eval; returning `None`
/// defers to it. Sync-only in Rust today — when async hosts need it, lift to a boxed future.
pub struct VerdictCtx<'a> {
    pub goal: &'a str,
    pub criteria: &'a [crate::harness::Criterion],
    pub attempt: u32,
    pub result: &'a str,
}
pub type VerdictFn = std::sync::Arc<dyn Fn(VerdictCtx<'_>) -> Option<Verdict> + Send + Sync>;

/// HarnessLoop — LLM-as-judge with feedback injection and skill extraction.
pub struct HarnessLoop<'a> {
    runner: &'a RuntimeRunner,
    eval_provider: Box<dyn LLMProvider>,
    max_attempts: usize,
    skill_dir: Option<std::path::PathBuf>,
    verdict_fn: Option<VerdictFn>,
}

impl<'a> HarnessLoop<'a> {
    pub fn new(
        runner: &'a RuntimeRunner,
        eval_provider: impl LLMProvider + 'static,
        max_attempts: usize,
        skill_dir: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            runner,
            eval_provider: Box::new(eval_provider),
            max_attempts,
            skill_dir,
            verdict_fn: None,
        }
    }

    /// I3.2 (A2/A3): plug in a host-supplied verdict closure. Returning `Some(Verdict)` skips the
    /// LLM eval; returning `None` defers. Pure addition — not setting it is byte-equivalent to
    /// the prior LLM-eval-only path.
    pub fn with_verdict_fn(mut self, f: VerdictFn) -> Self {
        self.verdict_fn = Some(f);
        self
    }

    pub fn run_streaming<'b>(
        &'b self,
        request: HarnessRequest,
    ) -> impl futures::Stream<Item = Result<HarnessEvent>> + 'b {
        use deepstrike_core::harness::eval::{build_eval_messages, parse_verdict, Criterion};

        async_stream::stream! {
            let mut current_goal = request.goal.clone();
            let criteria_texts: Vec<String> = request.criteria.iter().map(|c| c.text.clone()).collect();

            for attempt in 1..=self.max_attempts as u32 {
                let mut last_result = String::new();
                let mut last_iterations = 0u32;
                let mut last_total_tokens = 0u64;
                let mut last_status = "error".to_string();

                let goal_for_run = current_goal.clone();
                let stream_result = self.runner
                    .run_streaming(&goal_for_run, &criteria_texts, request.extensions.as_ref(), None)
                    .await;
                let mut stream = match stream_result {
                    Ok(s) => s,
                    Err(e) => { yield Err(e); return; }
                };

                while let Some(evt) = stream.next().await {
                    match evt {
                        Ok(RunEvent::TextDelta(d)) => {
                            last_result.push_str(&d);
                            yield Ok(HarnessEvent::Token(d));
                        }
                        Ok(RunEvent::ToolCall { id, name }) => yield Ok(HarnessEvent::ToolCall { id, name }),
                        Ok(RunEvent::ToolResult { call_id, content, is_error, .. }) => {
                            yield Ok(HarnessEvent::ToolResult { call_id, content, is_error });
                        }
                        Ok(RunEvent::Done { iterations, total_tokens, status }) => {
                            last_iterations = iterations;
                            last_total_tokens = total_tokens;
                            last_status = status;
                        }
                        Ok(_) => {}
                        Err(e) => { yield Err(e); return; }
                    }
                }

                yield Ok(HarnessEvent::Supervising);

                // I3.2 (A2/A3): host-supplied verdict_fn short-circuits the LLM eval. None ⇒ defer.
                let mut verdict: Option<Verdict> = None;
                let mut skill_candidate_from_eval = None;
                if let Some(f) = self.verdict_fn.clone() {
                    let ctx = VerdictCtx {
                        goal: &request.goal,
                        criteria: &request.criteria,
                        attempt,
                        result: &last_result,
                    };
                    verdict = f(ctx);
                }
                let verdict = if let Some(v) = verdict {
                    v
                } else {
                    // #6 (0.5.0): eval/verdict compute is the kernel's stateless free functions (was the
                    // EvalPipeline state machine). Build the eval prompt, call the eval LLM, parse the verdict.
                    let eval_criteria: Vec<Criterion> = request.criteria.iter().map(|c| Criterion {
                        text: c.text.clone(), required: c.required, weight: c.weight,
                    }).collect();
                    let messages = build_eval_messages(&request.goal, &eval_criteria, &last_result, attempt, true);

                    let mut eval_text = String::new();
                    let context = rendered_context_from_messages(messages);
                    let eval_state = self.eval_provider.create_run_state();
                    let mut eval_stream = match self.eval_provider.stream(&context, &[], None, eval_state.as_ref()).await {
                        Ok(s) => s,
                        Err(e) => { yield Err(e); return; }
                    };
                    while let Some(evt) = eval_stream.next().await {
                        if let Ok(StreamEvent::TextDelta { delta }) = evt {
                            eval_text.push_str(&delta);
                        }
                    }

                    let eval_result = parse_verdict(&eval_text);
                    skill_candidate_from_eval = eval_result.skill_candidate.clone();
                    Verdict {
                        passed: eval_result.passed,
                        overall_score: eval_result.overall_score,
                        feedback: eval_result.feedback.clone(),
                        details: eval_result.details.iter().map(|d| crate::harness::CriterionResult {
                            criterion: d.criterion.clone(), passed: d.passed, score: d.score, feedback: d.feedback.clone(),
                        }).collect(),
                    }
                };

                if verdict.passed {
                    if let Some(sc) = skill_candidate_from_eval {
                        if let Some(dir) = &self.skill_dir {
                            let mut fm = format!("---\nname: {}\ndescription: {}\n", sc.name, sc.description);
                            if let Some(wtu) = &sc.when_to_use { fm.push_str(&format!("when_to_use: {}\n", wtu)); }
                            fm.push_str("---\n\n");
                            fm.push_str(&sc.content);
                            if let Err(e) = tokio::fs::write(dir.join(format!("{}.md", sc.name)), fm).await {
                                yield Err(e.into()); return;
                            }
                        }
                    }
                    yield Ok(HarnessEvent::Done { verdict, iterations: last_iterations, total_tokens: last_total_tokens, status: last_status });
                    return;
                }

                yield Ok(HarnessEvent::Revising { verdict: verdict.clone() });
                current_goal = format!("{}\n\n[Attempt {} feedback: {}]", request.goal, attempt, verdict.feedback);
            }

            yield Ok(HarnessEvent::MaxAttemptsReached);
        }
    }
}
