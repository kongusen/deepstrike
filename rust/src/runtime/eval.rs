//! `judge()` — one-shot quality scoring against a goal + criteria.
//!
//! Rust port of node/src/runtime/eval.ts. Wraps the kernel's `gen_eval` free functions
//! (re-exported from `deepstrike_core::harness::eval`) into a small surface for callers
//! that just want "does this result meet the criteria?" without standing up
//! `HarnessLoop`.

use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::harness::eval::{
    build_eval_messages as core_build_eval_messages, parse_verdict as core_parse_verdict,
    verdict_output_schema as core_verdict_output_schema, EvalResult,
};
use deepstrike_core::types::message::{Content, Message, Role};
use futures::StreamExt;

use crate::providers::{LLMProvider, StreamEvent};
use crate::Result;

/// Re-export the kernel's `Criterion` so callers don't need to import from `deepstrike_core`.
pub use deepstrike_core::harness::eval::Criterion;

/// The verdict shape parsed from the judge LLM. Same struct as the kernel's `EvalResult`.
pub type Verdict = EvalResult;

/// Build the kernel's eval prompt for (goal, criteria, result). Pure — does not call an LLM.
pub fn build_eval_messages(goal: &str, criteria: &[Criterion], result: &str) -> Vec<Message> {
    core_build_eval_messages(goal, criteria, result, 1, false)
}

/// Parse a Verdict from raw judge-LLM text.
pub fn parse_verdict(text: &str) -> Verdict {
    core_parse_verdict(text)
}

/// The JSON Schema the kernel expects judge output to conform to.
pub fn verdict_output_schema() -> serde_json::Value {
    core_verdict_output_schema(false)
}

/// One-shot judge: render the eval prompt, stream the provider, parse the verdict.
/// Errors when the provider returns no text.
pub async fn judge(
    provider: &dyn LLMProvider,
    goal: &str,
    criteria: &[Criterion],
    result: &str,
) -> Result<Verdict> {
    let msgs = build_eval_messages(goal, criteria, result);
    let system_text: String = msgs
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(|m| match &m.content {
            Content::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let turns: Vec<Message> = msgs.into_iter().filter(|m| m.role != Role::System).collect();
    let ctx = RenderedContext {
        system_text,
        system_stable: String::new(),
        system_knowledge: String::new(),
        state_turn: None,
        turns,
        frozen_prefix_len: None,
    };

    let mut text = String::new();
    let mut stream = provider.stream(&ctx, &[], None, None).await?;
    while let Some(evt) = stream.next().await {
        if let Ok(StreamEvent::TextDelta { delta }) = evt {
            text.push_str(&delta);
        }
    }
    if text.is_empty() {
        return Err(crate::Error::Other("judge: provider produced no text".to_string()));
    }
    Ok(parse_verdict(&text))
}
