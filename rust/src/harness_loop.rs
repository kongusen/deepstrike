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

/// HarnessLoop — LLM-as-judge with feedback injection and skill extraction.
pub struct HarnessLoop<'a> {
    runner: &'a RuntimeRunner,
    eval_provider: Box<dyn LLMProvider>,
    max_attempts: usize,
    skill_dir: Option<std::path::PathBuf>,
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
        }
    }

    pub fn run_streaming<'b>(
        &'b self,
        request: HarnessRequest,
    ) -> impl futures::Stream<Item = Result<HarnessEvent>> + 'b {
        use deepstrike_core::harness::eval_pipeline::{
            EvalAction, EvalEvent, EvalPipeline, EvalPolicy,
        };

        async_stream::stream! {
            let mut pipeline = EvalPipeline::new(EvalPolicy { extract_skill_on_pass: true });
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

                let eval_action = pipeline.feed(EvalEvent::Outcome {
                    goal: request.goal.clone(),
                    criteria: request.criteria.iter().map(|c| deepstrike_core::harness::eval_pipeline::Criterion {
                        text: c.text.clone(), required: c.required, weight: c.weight,
                    }).collect(),
                    result: last_result,
                    attempt,
                });
                let messages = match eval_action {
                    EvalAction::Evaluate { messages } => messages,
                    EvalAction::Done { .. } => break,
                };

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

                let eval_result = match pipeline.feed(EvalEvent::EvalResult { content: eval_text }) {
                    EvalAction::Done { result } => result,
                    _ => break,
                };

                let verdict = Verdict {
                    passed: eval_result.passed,
                    overall_score: eval_result.overall_score,
                    feedback: eval_result.feedback.clone(),
                    details: eval_result.details.iter().map(|d| crate::harness::CriterionResult {
                        criterion: d.criterion.clone(), passed: d.passed, score: d.score, feedback: d.feedback.clone(),
                    }).collect(),
                };

                if verdict.passed {
                    if let Some(sc) = eval_result.skill_candidate {
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
                pipeline.reset();
            }

            yield Ok(HarnessEvent::MaxAttemptsReached);
        }
    }
}
