use deepstrike_core::context::token_engine::ContextTokenEngine;
use deepstrike_core::context::units::unit_boundaries;
use deepstrike_core::context::utility::{UtilitySelectionContext, plan_utility_archive};
use deepstrike_core::types::message::Message;

fn unit(user: &str, assistant: &str) -> Vec<Message> {
    let mut user = Message::user(user);
    user.token_count = Some(40);
    let mut assistant = Message::assistant(assistant);
    assistant.token_count = Some(40);
    vec![user, assistant]
}

#[test]
fn utility_selector_retains_old_high_value_unit_that_fifo_discards() {
    let messages = [
        unit(
            "ORCHID release criterion",
            "DECISION: retry after failure; artifact /work/orchid.json",
        ),
        unit("routine chatter one", "acknowledged"),
        unit("routine chatter two", "acknowledged"),
        unit("latest request", "working on it"),
    ]
    .concat();
    let engine = ContextTokenEngine::char_approx();
    let units = unit_boundaries(&messages);
    let fifo_archived = units[..2]
        .iter()
        .flat_map(|range| messages[range.clone()].iter())
        .map(|message| message.content.as_text().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    let plan = plan_utility_archive(
        &messages,
        320,
        160,
        1,
        &engine,
        &UtilitySelectionContext {
            goal: "ship ORCHID release",
            criteria: &["preserve the retry decision and artifact".into()],
            preserved_refs: &[],
            active_directives: &[],
        },
    );
    let retained = plan
        .retained_ranges
        .iter()
        .flat_map(|range| messages[range.clone()].iter())
        .map(|message| message.content.as_text().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!fifo_archived.is_empty() && fifo_archived.contains("ORCHID"));
    assert!(retained.contains("ORCHID"));
    assert!(retained.contains("/work/orchid.json"));
    assert!(!retained.contains("routine chatter"));
    assert_eq!(plan.retained_tokens, 160);
}
