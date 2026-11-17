use super::*;

/// Plan task 8.0 · the ledger attributes real render changes: a compaction and a changed
/// toolset. Old turns leaving the protected window render unchanged, so plain appends keep the
/// prefix.
#[test]
fn the_cache_ledger_attributes_render_breaks() {
    use crate::context::cache_ledger::BreakCause;
    let mut mgr = ContextManager::new(100_000);
    let tools = |names: &[&str]| -> Vec<ToolSchema> {
        names
            .iter()
            .map(|name| ToolSchema {
                name: CompactString::new(*name),
                description: String::new(),
                parameters: serde_json::json!({ "type": "object" }),
            })
            .collect()
    };
    let toolset = tools(&["read"]);
    let step = |mgr: &mut ContextManager, n: usize| {
        let mut call = CoreMessage::assistant(format!(
            "Step {n}: I will now read the next section of the ledger to continue the audit."
        ));
        call.tool_calls = vec![crate::types::message::ToolCall {
            id: format!("c{n}").into(),
            name: "read".into(),
            arguments: serde_json::json!({ "page": n }),
        }];
        mgr.push_history(call, 20);
        mgr.push_history(tool_result_msg(&format!("c{n}"), "page body"), 5);
    };
    mgr.push_history(CoreMessage::user("audit the ledger"), 5);
    step(&mut mgr, 1);
    assert_eq!(mgr.note_request_sent(&mgr.render(), &toolset, 1), None, "nothing to compare yet");
    step(&mut mgr, 2);
    assert_eq!(mgr.note_request_sent(&mgr.render(), &toolset, 2), None, "an append keeps the prefix");
    step(&mut mgr, 3);
    assert_eq!(
        mgr.note_request_sent(&mgr.render(), &toolset, 3),
        None,
        "step 1 left the protected window and still renders as it was sent"
    );

    step(&mut mgr, 4);
    mgr.cache_ledger.note_cause(BreakCause::of_compaction(PressureAction::AutoCompact));
    mgr.partitions.history.truncate(1);
    let found = mgr.note_request_sent(&mgr.render(), &toolset, 4).expect("compaction breaks");
    assert_eq!(found.cause, BreakCause::Auto, "an announced cause wins over inference");

    let found = mgr
        .note_request_sent(&mgr.render(), &tools(&["read", "write"]), 5)
        .expect("a new toolset breaks everything");
    assert_eq!((found.cause, found.kept_tokens), (BreakCause::Toolset, 0));

    let metrics = mgr.lifecycle_metrics();
    assert_eq!(metrics.prefix_breaks.len(), 2);
    assert_eq!(
        metrics.cache_loss_tokens,
        metrics.prefix_breaks.iter().map(|b| u64::from(b.lost_tokens)).sum::<u64>(),
        "cache_loss_tokens sums every break"
    );
}
use crate::context::task_state::PlanStep;
use crate::types::message::CoreMessage;
use crate::types::skill::SkillMetadata;

#[test]
fn low_provider_usage_does_not_hide_resident_history_pressure() {
    let mut mgr = ContextManager::with_config(
        1_000,
        ContextConfig::default(),
        ContextTokenEngine::char_approx(),
    );
    mgr.push_history(CoreMessage::user("large history"), 1_100);
    mgr.set_observed_prompt_tokens(10);
    assert_eq!(mgr.should_compress(), PressureAction::AutoCompact);
    mgr.compress(PressureAction::AutoCompact);
    assert!(!mgr.should_renew());
}

fn staged_text(mgr: &ContextManager) -> String {
    mgr.render()
        .state_turn
        .and_then(|turn| turn.content.as_text().map(str::to_string))
        .unwrap_or_default()
}

#[test]
fn lifecycle_metrics_count_regret_waste_and_cache_loss() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("alpha notes"), 7, false);
    mgr.push_knowledge_entry(Some("memory:b".into()), CoreMessage::system("beta notes"), 5, false);
    mgr.push_history(CoreMessage::user("tell me about memory:b"), 10);
    assert!(mgr.remove_knowledge("memory:a"));
    mgr.sweep_knowledge_at_boundary();
    // memory:a left unreferenced (waste) and comes back (regret).
    mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("alpha notes"), 7, false);
    let metrics = mgr.lifecycle_metrics();
    assert_eq!(
        (metrics.knowledge_entries, metrics.knowledge_referenced, metrics.regret),
        (2, 1, 1)
    );
    assert_eq!(metrics.waste_tokens, 7 + 7, "evicted unreferenced + resident unreferenced");
    assert_eq!(metrics.cache_loss_tokens, 0);
    let items: Vec<_> = metrics
        .items
        .iter()
        .map(|item| (item.key.as_deref().unwrap(), item.end, item.value.paged_in_after_evict))
        .collect();
    assert_eq!(
        items,
        vec![
            ("memory:a", Some(ItemEnd::Removed), false),
            ("memory:b", None, false),
            ("memory:a", None, true),
        ]
    );
}

#[test]
fn the_item_graph_records_answers_references_reads_back_and_waste() {
    use crate::types::message::ToolCall;
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_knowledge_entry(Some("project:orchid".into()), CoreMessage::system("orchid storage layout"), 6, false);
    mgr.push_recalled(vec![
        ("r1".into(), "[MEMORY record_id=r1 kind=project] mercury deploy freeze".into()),
        ("r2".into(), "[MEMORY record_id=r2 kind=project] office wifi rotation".into()),
    ]);
    let ids: Vec<ItemId> = mgr.partitions.knowledge.entries.iter().map(|entry| entry.id).collect();
    assert_eq!(ids, vec![1, 2, 3], "knowledge items draw from the item allocator");
    let (orchid, r1, r2) = (ids[0], ids[1], ids[2]);

    mgr.push_history(CoreMessage::user("question"), 5);
    let mut call = CoreMessage::assistant("reading");
    call.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: "read".into(),
        arguments: serde_json::json!({ "key": "project:orchid" }),
    }];
    mgr.push_history(call, 10);
    mgr.push_history(tool_result_msg("c1", "the layout"), 10);
    let tag = |index: usize| mgr.partitions.history.tag(index).id;
    let (assistant, result) = (tag(1), tag(2));
    assert!(assistant > r2, "history and knowledge share one allocator");
    mgr.note_read_back("c1");
    mgr.note_memory_read("r1");
    mgr.note_memory_read("no-such-record");

    let metrics = mgr.lifecycle_metrics();
    let edges: Vec<_> = metrics.edges.iter().map(|e| (e.from, e.to, e.relation)).collect();
    assert_eq!(
        edges,
        vec![
            (assistant, orchid, Relation::ReferencesStrong),
            (result, assistant, Relation::Answers),
            (assistant, result, Relation::ReadsBack),
            (assistant, r1, Relation::ReadsBack),
        ]
    );
    assert_eq!(metrics.waste_items, vec![r2], "r1 was read back, orchid referenced");
    let recorded: Vec<_> = metrics.items.iter().map(|item| item.id).collect();
    assert_eq!(recorded, ids);
}

#[test]
fn host_output_and_memory_hits_do_not_count_as_use_but_the_models_words_do() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_recalled(vec![
        ("vat".into(), "[MEMORY record_id=vat kind=project] tax ledger VAT rounding per transaction".into()),
    ]);
    let refs = |mgr: &ContextManager| mgr.partitions.knowledge.entries[0].value.ref_count;
    // A ledger page and a search hit share the record's words: recorded, not counted.
    mgr.push_history(tool_result_msg("c1", "transaction ledger page 1: rounding totals"), 10);
    mgr.push_history(
        CoreMessage::user("[MEMORY record_ref=other kind=project] the tax ledger rounds each transaction"),
        10,
    );
    assert_eq!(refs(&mgr), 0);
    assert_eq!(mgr.graph.edges().iter().filter(|e| e.relation == Relation::ReferencesLexical).count(), 2);
    mgr.push_history(CoreMessage::assistant("VAT rounding in the tax ledger applies here"), 10);
    assert_eq!(refs(&mgr), 1, "the model's own words are evidence");
}

#[test]
fn using_a_memory_hits_content_reaches_the_recall_it_expands() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_recalled(vec![(
        "flag-rule".into(),
        "[MEMORY record_id=flag-rule kind=project] ledger audit: which flags count".into(),
    )]);
    let recalled = mgr.partitions.knowledge.entries[0].id;
    mgr.push_history(
        CoreMessage::user("[MEMORY record_ref=flag-rule kind=project] REVIEW-HOLD counts as REVIEW in audit totals"),
        10,
    );
    let hit = mgr.partitions.history.tag(mgr.partitions.history.len() - 1).id;
    assert!(mgr.graph.edges().contains(&ItemEdge { from: hit, to: recalled, relation: Relation::Expands, turn: 0 }));
    assert_eq!(mgr.partitions.knowledge.entries[0].value.ref_count, 0);
    mgr.push_history(CoreMessage::assistant("T0042 is REVIEW-HOLD, so it counts toward the totals"), 10);
    let used = mgr.partitions.history.tag(mgr.partitions.history.len() - 1).id;
    assert!(mgr.graph.edges().iter().any(|e| e.from == used && e.to == hit));
    assert_eq!(mgr.partitions.knowledge.entries[0].value.ref_count, 1, "one hop along expands");
}

#[test]
fn an_item_a_resident_item_names_neither_expires_nor_yields_to_the_budget() {
    use crate::context::item::RECALL_LEASE_TURNS;
    let mut mgr = ContextManager::new(1_000);
    mgr.config.knowledge_budget_ratio = 0.01;
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_recalled(vec![
        ("kept".into(), "[MEMORY record_id=kept kind=project] orchid storage layout".into()),
        ("idle".into(), "[MEMORY record_id=idle kind=project] mercury deploy freeze".into()),
    ]);
    let mut call = CoreMessage::assistant("reading");
    call.tool_calls = vec![crate::types::message::ToolCall {
        id: "c1".into(),
        name: "memory".into(),
        arguments: serde_json::json!({ "record_id": "kept" }),
    }];
    mgr.push_history(call, 10);
    mgr.enforce_knowledge_budget();
    let leaving = |mgr: &ContextManager| -> Vec<_> {
        mgr.partitions.knowledge.entries.iter().map(|e| (e.key.clone().unwrap(), e.is_leaving())).collect()
    };
    assert_eq!(leaving(&mgr), vec![("memory:kept".into(), false), ("memory:idle".into(), true)]);
    mgr.sweep_expired_item_leases(10 * RECALL_LEASE_TURNS);
    assert!(!mgr.partitions.knowledge.entries[0].is_leaving(), "held by the resident call");
}

#[test]
fn a_child_reports_which_inherited_parent_items_it_used_and_what_it_read_from_dependencies() {
    use crate::types::message::ToolCall;
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    let parent = |item| ExternalItem { operation: Some("op-parent".into()), item, ..ExternalItem::default() };
    mgr.push_knowledge_entry(Some("style".into()), CoreMessage::system("house style forbids bullets"), 4, true);
    mgr.note_inherited_from(Some("style"), parent(7));
    mgr.push_knowledge_entry(Some("notes".into()), CoreMessage::system("orchid storage layout"), 4, false);
    mgr.note_inherited_from(Some("notes"), parent(9));
    let mut call = CoreMessage::assistant("no bullets per the house style");
    call.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: "read_result".into(),
        arguments: serde_json::json!({ "call_id": "dep:wf-node0" }),
    }];
    mgr.push_history(call, 10);
    let reader = mgr.partitions.history.tag(0).id;
    mgr.note_read_back("dep:wf-node0");

    let metrics = mgr.lifecycle_metrics();
    assert_eq!(metrics.parent_items_used, vec![parent(7)], "the notes copy went unused");
    let relations: Vec<_> = metrics.external_edges.iter().map(|e| (e.from, e.relation)).collect();
    let (style, notes) = (mgr.partitions.knowledge.entries[0].id, mgr.partitions.knowledge.entries[1].id);
    assert_eq!(
        relations,
        vec![
            (style, Relation::InheritedFrom),
            (notes, Relation::InheritedFrom),
            (reader, Relation::DependsOn),
        ]
    );
    assert_eq!(metrics.external_edges[2].to.agent.as_deref(), Some("wf-node0"));
}

#[test]
fn a_recall_left_unused_expires_and_leaves_at_the_next_boundary() {
    use crate::context::item::RECALL_LEASE_TURNS;
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.sweep_expired_item_leases(1);
    mgr.push_recalled(vec![
        ("r1".into(), "[MEMORY record_id=r1 kind=project] orchid storage".into()),
        ("r2".into(), "[MEMORY record_id=r2 kind=project] mercury deploys".into()),
    ]);
    mgr.sweep_expired_item_leases(4);
    assert!(mgr.note_memory_expanded("r2"));
    mgr.sweep_expired_item_leases(1 + RECALL_LEASE_TURNS);
    let leaving: Vec<_> = mgr
        .partitions
        .knowledge
        .entries
        .iter()
        .map(|entry| entry.leaving)
        .collect();
    assert_eq!(leaving, vec![Some(ItemEnd::Expired), None], "r2 was used at turn 4");
    // Still rendered until the boundary: an expiry never rewrites cached bytes mid-generation.
    assert!(mgr.render().system_knowledge.contains("orchid storage"));
    mgr.sweep_knowledge_at_boundary();
    assert!(!mgr.render().system_knowledge.contains("orchid storage"));

    let metrics = mgr.lifecycle_metrics();
    assert_eq!((metrics.recalled, metrics.recall_expanded), (2, 1));
    assert_eq!(metrics.items[0].end, Some(ItemEnd::Expired));
    assert_eq!(metrics.items[0].origin, ItemOrigin::MemoryRecall);
}

#[test]
fn a_skill_body_leaves_when_its_skill_is_deactivated_even_if_pinned() {
    use crate::context::item::{Lease, LeaseEvent};
    let mut mgr = ContextManager::new(100_000);
    mgr.activate_skill("debug");
    mgr.push_knowledge_entry(
        Some("skill:debug".into()),
        CoreMessage::system("debug skill body"),
        10,
        true,
    );
    let skill = &mgr.partitions.knowledge.entries[0];
    assert_eq!(skill.lease, Lease::Until(LeaseEvent::SkillDeactivated));
    mgr.sweep_expired_item_leases(1_000);
    assert!(!mgr.partitions.knowledge.entries[0].is_leaving(), "no turn limit");

    assert!(mgr.deactivate_skill("debug"));
    assert_eq!(mgr.partitions.knowledge.entries[0].leaving, Some(ItemEnd::Expired));
    mgr.sweep_knowledge_at_boundary();
    assert!(mgr.partitions.knowledge.is_empty());
    assert_eq!(mgr.lifecycle_metrics().items[0].end, Some(ItemEnd::Expired));
}

#[test]
fn a_full_child_inherits_the_parents_knowledge_but_not_its_own_state() {
    use crate::types::agent::ContextInheritance as I;
    let mut mgr = ContextManager::new(100_000);
    mgr.push_knowledge_entry(Some("style".into()), CoreMessage::system("no bullets"), 3, true);
    mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1] orchid".into())]);
    mgr.activate_skill("debug");
    mgr.push_knowledge_entry(Some("skill:debug".into()), CoreMessage::system("body"), 3, true);
    mgr.push_knowledge_item(
        ItemOrigin::System,
        Some("governance:denied-tools".into()),
        CoreMessage::system("denied"),
        2,
        true,
    );
    mgr.push_knowledge_entry(Some("gone".into()), CoreMessage::system("leaving"), 2, false);
    mgr.remove_knowledge("gone");

    let keys = |inheritance| -> Vec<Option<String>> {
        mgr.inheritable_knowledge(inheritance).into_iter().map(|entry| entry.0).collect()
    };
    assert_eq!(keys(I::Full), vec![Some("style".to_string()), Some("memory:r1".to_string())]);
    assert!(keys(I::SystemOnly).is_empty());
    assert!(keys(I::None).is_empty());
    assert!(mgr.inheritable_knowledge(I::Full)[0].3, "a pin is inherited");
}

#[test]
fn the_report_lists_history_items_and_why_each_left() {
    use crate::context::item::HISTORY_ITEM_DETAIL_CAP;
    let mut mgr = ContextManager::new(10_000);
    for i in 0..6 {
        mgr.push_history(CoreMessage::user(format!("message {i} ").repeat(80)), 400);
    }
    let (_, _, archived, _) = mgr.force_compress();
    assert!(!archived.is_empty());
    mgr.truncate_history(mgr.partitions.history.len() - 1);
    mgr.push_history(CoreMessage::user("after rollback"), 5);
    mgr.renew();

    let metrics = mgr.lifecycle_metrics();
    let ends: Vec<_> = metrics.history_items.iter().map(|item| item.end).collect();
    assert!(ends.contains(&Some(ItemEnd::Compacted)), "{ends:?}");
    assert!(ends.contains(&Some(ItemEnd::Removed)), "{ends:?}");
    let mut ids: Vec<_> = metrics.history_items.iter().map(|item| item.id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), metrics.history_items.len(), "each item is listed once");
    assert_eq!(metrics.history_items_omitted, 0);

    // Past the cap only the latest items are listed; the rest are counted.
    let mut long = ContextManager::new(1_000_000);
    for i in 0..HISTORY_ITEM_DETAIL_CAP + 10 {
        long.push_history(CoreMessage::user(format!("m{i}")), 1);
    }
    let metrics = long.lifecycle_metrics();
    assert_eq!(metrics.history_items.len(), HISTORY_ITEM_DETAIL_CAP);
    assert_eq!(metrics.history_items_omitted, 10);
    assert_eq!(metrics.history_items.last().unwrap().id, (HISTORY_ITEM_DETAIL_CAP + 10) as u64);
}

#[test]
fn the_estimate_calibrates_toward_what_the_provider_counts() {
    let mut mgr = ContextManager::new(100_000);
    assert_eq!(mgr.note_request_estimate(600), 600, "uncalibrated before any count");
    mgr.set_observed_prompt_tokens(810);
    assert_eq!(mgr.note_request_estimate(1_200), 1_410, "one sample: its offset");
    mgr.set_observed_prompt_tokens(1_470);
    // A fixed overhead plus a proportional gap (count = 1.1 x estimate + 150) is fitted
    // exactly once there are two sizes.
    for raw in [2_000u32, 2_800] {
        mgr.note_request_estimate(raw);
        mgr.set_observed_prompt_tokens(raw * 11 / 10 + 150);
    }
    assert_eq!(mgr.note_request_estimate(3_600), 4_110);
    // An observation with no estimate pending (a restore, a test) adds no sample.
    let samples = mgr.estimate_samples.len();
    mgr.last_request_estimate = 0;
    mgr.set_observed_prompt_tokens(50);
    assert_eq!(mgr.estimate_samples.len(), samples);
    // Only the most recent samples count.
    for _ in 0..ESTIMATE_CALIBRATION_SAMPLES {
        mgr.note_request_estimate(1_000);
        mgr.set_observed_prompt_tokens(1_000);
    }
    assert_eq!(mgr.note_request_estimate(5_000), 5_000);
}

#[test]
fn replay_sent_back_with_a_turn_counts_toward_the_estimate() {
    let mut mgr = ContextManager::new(100_000);
    mgr.push_history(CoreMessage::user("question"), 5);
    let before = (mgr.render().estimated_tokens, mgr.rho());
    mgr.pending_assistant_replay =
        Some(serde_json::json!({ "reasoning_content": "step ".repeat(400) }));
    mgr.push_history(CoreMessage::assistant("answer"), 5);
    let (estimate, rho) = (mgr.render().estimated_tokens, mgr.rho());
    assert!(estimate >= before.0 + 5 + 300, "{estimate} vs {}", before.0);
    assert!(rho > before.1);
}

#[test]
fn calibration_raises_pressure_but_never_lowers_it() {
    let mut mgr = ContextManager::new(1_000);
    mgr.push_history(CoreMessage::user("x".repeat(400)), 400);
    let raw = mgr.pressure.pressure(&mgr.partitions, &mgr.engine, None);
    mgr.estimate_samples = [(1_000, 500)].into();
    assert!((mgr.rho() - raw).abs() < 1e-9, "a low count does not delay compaction");
    mgr.estimate_samples = [(1_000, 1_200)].into();
    assert!((mgr.rho() - (raw + 0.2)).abs() < 1e-3, "an undercount raises pressure");
}

#[test]
fn a_provider_summary_replaces_the_items_it_covered() {
    use crate::types::message::ToolCall;
    let mut mgr = ContextManager::new(100_000);
    mgr.push_history(CoreMessage::user("question"), 50);
    let mut call = CoreMessage::assistant("reading");
    call.tool_calls = vec![ToolCall {
        id: "c1".into(),
        name: "read".into(),
        arguments: serde_json::json!({}),
    }];
    mgr.push_history(call, 20);
    mgr.push_history(tool_result_msg("c1", &"body ".repeat(200)), 400);
    mgr.push_history(CoreMessage::user("next question"), 10);
    let (assistant, tool) = (mgr.partitions.history.tag(1).id, mgr.partitions.history.tag(2).id);

    let block = serde_json::json!({ "protocol": "anthropic-messages", "native_blocks": [{ "type": "compaction" }] });
    // Covering through the call extends to its result: a transaction is never split.
    assert!(mgr.absorb_provider_compaction(assistant, block.clone(), 30));
    let tags = mgr.partitions.history.tags();
    assert_eq!(tags[0].origin, ItemOrigin::ProviderSummary);
    assert_eq!(tags.len(), 2, "summary + the uncovered question");
    assert!(tags.iter().all(|tag| tag.id != tool));
    assert_eq!(mgr.partitions.history.token_count(), 30 + 10);
    assert!(mgr.handles.residency_for_source("c1").is_none(), "the covered result's handle left");

    let rendered = mgr.render();
    let summary = rendered.turn_items.iter().position(|id| *id == tags[0].id).unwrap();
    assert_eq!(rendered.turn_replays[summary], Some(block));
    let ends: Vec<_> = mgr.lifecycle_metrics().history_items.iter().map(|item| item.end).collect();
    assert_eq!(ends.iter().filter(|end| **end == Some(ItemEnd::Compacted)).count(), 3);
    let summarized: Vec<_> = mgr
        .graph
        .edges()
        .iter()
        .filter(|edge| edge.relation == Relation::Summarizes)
        .map(|edge| (edge.from, edge.to))
        .collect();
    assert_eq!(summarized.len(), 3, "the summary stands in for each item it covered");
    assert!(summarized.iter().all(|(from, _)| *from == tags[0].id));
    assert!(summarized.contains(&(tags[0].id, tool)));
    assert!(!mgr.absorb_provider_compaction(9_999, serde_json::json!({}), 1), "unknown item");
}

#[test]
fn tool_results_the_provider_cleared_render_as_handles() {
    let mut mgr = ContextManager::new(100_000);
    mgr.push_history(tool_result_msg("c1", "first"), 5);
    mgr.push_history(tool_result_msg("c2", "second"), 5);
    mgr.absorb_cleared_tool_results(["c1"]);
    assert_eq!(mgr.handles.residency_for_source("c1"), Some(&Residency::Collapsed));
    assert_eq!(mgr.handles.residency_for_source("c2"), Some(&Residency::Resident));
    assert_eq!(mgr.history_fidelity(0), crate::context::item::Fidelity::Handle);
}

#[test]
fn history_messages_are_items_whose_identity_survives_renewal() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_history(CoreMessage::user("question"), 5);
    let mut call = CoreMessage::assistant("reading");
    call.tool_calls.push(crate::types::message::ToolCall {
        id: "call-1".into(),
        name: "read".into(),
        arguments: serde_json::json!({}),
    });
    mgr.push_history(call, 5);
    mgr.push_history(
        CoreMessage::tool(vec![ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "body".into(),
            is_error: false,
            durable_content: None,
        }]),
        7,
    );
    let tags: Vec<_> = (0..3).map(|i| mgr.partitions.history.tag(i)).collect();
    assert_eq!(tags.iter().map(|tag| tag.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(
        tags.iter().map(|tag| tag.origin).collect::<Vec<_>>(),
        vec![ItemOrigin::User, ItemOrigin::Assistant, ItemOrigin::ToolResult]
    );
    mgr.note_read_back("call-1");
    mgr.note_read_back("dep:wf-node0");

    mgr.renew();
    let carried: Vec<_> = (0..mgr.partitions.history.len())
        .map(|i| mgr.partitions.history.tag(i).id)
        .collect();
    assert!(carried.iter().all(|id| *id > 0), "renewal carries identity: {carried:?}");
    mgr.push_history(CoreMessage::user("next"), 3);
    let last = mgr.partitions.history.len() - 1;
    assert_eq!(mgr.partitions.history.tag(last).id, 4, "ids are never reused");

    let history = mgr.lifecycle_metrics().history;
    let tool = history.iter().find(|tally| tally.origin == ItemOrigin::ToolResult).unwrap();
    assert_eq!((tool.items, tool.tokens, tool.read_back), (1, 7, 1));
    let dependency = history
        .iter()
        .find(|tally| tally.origin == ItemOrigin::DependencyOutput)
        .unwrap();
    assert_eq!(dependency.read_back, 1);
    let user = history.iter().find(|tally| tally.origin == ItemOrigin::User).unwrap();
    assert_eq!(user.items, 2);
}

#[test]
fn a_fresh_recall_of_a_resident_record_renews_its_lease() {
    use crate::context::item::RECALL_LEASE_TURNS;
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    let line = || vec![("r1".to_string(), "[MEMORY record_id=r1 kind=project] a".to_string())];
    mgr.push_recalled(line());
    mgr.sweep_expired_item_leases(5);
    mgr.push_recalled(line());
    mgr.sweep_expired_item_leases(RECALL_LEASE_TURNS);
    assert!(!mgr.partitions.knowledge.entries[0].is_leaving());
    mgr.sweep_expired_item_leases(5 + RECALL_LEASE_TURNS);
    assert!(mgr.partitions.knowledge.entries[0].is_leaving());
}

#[test]
fn the_recall_index_line_matches_the_shared_vector() {
    assert_eq!(
        ContextManager::recall_index_line("record-v", "project", "untrusted", "how briefs are laid out", "fact"),
        "[MEMORY record_id=record-v name=fact kind=project trust=untrusted] how briefs are laid out"
    );
    assert_eq!(
        ContextManager::recall_index_line("record-v", "project", "untrusted", " ", "fact"),
        "[MEMORY record_id=record-v name=fact kind=project trust=untrusted] fact"
    );
}

#[test]
fn recall_is_owed_at_start_and_after_each_renewal_and_dedupes_by_record() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    assert_eq!(mgr.take_due_recall(), None, "recall is off unless enabled");
    mgr.enable_recall(3);
    mgr.init_task("find the bug".to_string(), vec![]);
    assert_eq!(mgr.take_due_recall(), Some(("find the bug".to_string(), 3)));
    assert_eq!(mgr.take_due_recall(), None, "one recall per boundary");
    mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1 kind=project] a".into())]);
    mgr.push_recalled(vec![("r1".into(), "[MEMORY record_id=r1 kind=project] changed".into())]);
    assert_eq!(mgr.partitions.knowledge.len(), 1);
    mgr.renew();
    assert!(mgr.take_due_recall().is_some());
}

#[test]
fn knowledge_before_the_first_send_goes_straight_into_system_knowledge() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_history(CoreMessage::user("goal"), 10);
    mgr.push_knowledge_entry(Some("memory:a".into()), CoreMessage::system("recall a"), 5, false);
    assert!(mgr.render().system_knowledge.contains("recall a"));
    assert!(!staged_text(&mgr).contains("recall a"));
}

#[test]
fn knowledge_after_the_send_is_staged_and_keeps_the_cached_prefix() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_history(CoreMessage::user("goal ".repeat(200)), 400);
    let before = mgr.render();
    mgr.mark_prefix_sent();
    mgr.push_knowledge_entry(Some("skill:x".into()), CoreMessage::system("skill body"), 20, false);
    let after = mgr.render();
    assert_eq!(after.system_stable, before.system_stable);
    assert_eq!(after.system_knowledge, before.system_knowledge, "system[1] bytes unchanged");
    assert_eq!(after.turns.len(), before.turns.len());
    assert!(staged_text(&mgr).contains("skill body"));
    assert_eq!(mgr.cache_invalidated_tokens(), 0);

    // The next boundary promotes it into system[1].
    mgr.sweep_knowledge_at_boundary();
    assert!(mgr.render().system_knowledge.contains("skill body"));
    assert!(!staged_text(&mgr).contains("skill body"));
}

#[test]
fn large_staged_knowledge_is_promoted_at_once_and_the_invalidation_is_counted() {
    let mut mgr = ContextManager::new(100_000);
    mgr.init_task("goal".to_string(), vec![]);
    mgr.push_history(CoreMessage::user("short"), 40);
    mgr.mark_prefix_sent();
    mgr.push_knowledge_entry(Some("ref:big".into()), CoreMessage::system("big reference"), 10, false);
    assert!(mgr.render().system_knowledge.contains("big reference"));
    assert_eq!(mgr.cache_invalidated_tokens(), 40);
    // The promotion rewrote system[1]; the next append before a send is free again.
    mgr.push_knowledge_entry(Some("ref:next".into()), CoreMessage::system("next"), 1, false);
    assert!(mgr.render().system_knowledge.contains("next"));
}

#[test]
fn manager_renew_advances_sprint_and_keeps_goal() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("test goal".to_string(), vec![]);
    mgr.partitions.system.push(CoreMessage::system("rules"), 10);
    for i in 0..10 {
        mgr.push_history(CoreMessage::user(format!("msg {i}")), 50);
    }
    mgr.renew();
    assert_eq!(mgr.partitions.task_state.goal, "test goal");
    assert_eq!(mgr.sprint, 1);
}

#[test]
fn compress_only_touches_history() {
    let mut mgr = ContextManager::new(1_000);
    mgr.push_knowledge(CoreMessage::system("knowledge content"), 100);
    for _ in 0..30 {
        mgr.push_history(CoreMessage::user("history msg"), 50);
    }
    let knowledge_before = mgr.partitions.knowledge.token_count;
    let history_before = mgr.partitions.history.token_count();
    mgr.compress(PressureAction::AutoCompact);
    assert_eq!(mgr.partitions.knowledge.token_count, knowledge_before);
    assert!(mgr.partitions.history.token_count() < history_before);
}

#[test]
fn init_task_sets_goal_and_criteria() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("analyse data".to_string(), vec!["criterion A".to_string()]);
    assert_eq!(mgr.partitions.task_state.goal, "analyse data");
    assert_eq!(mgr.partitions.task_state.criteria, ["criterion A"]);
}

#[test]
fn update_task_applies_plan() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("g".to_string(), vec![]);
    mgr.update_task(TaskUpdate {
        plan: Some(vec!["step 1".to_string(), "step 2".to_string()]),
        current_step: Some(0),
        ..Default::default()
    });
    assert_eq!(mgr.partitions.task_state.plan.len(), 2);
    assert_eq!(mgr.partitions.task_state.current_step, Some(0));
}

#[test]
fn task_state_survives_autocompact() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("survive compression".to_string(), vec![]);
    mgr.update_task(TaskUpdate {
        plan: Some(vec!["fetch data".to_string(), "analyse".to_string()]),
        ..Default::default()
    });
    for _ in 0..10 {
        mgr.push_history(CoreMessage::user("filler"), 50);
    }
    mgr.compress(PressureAction::AutoCompact);
    assert_eq!(mgr.partitions.task_state.goal, "survive compression");
    assert_eq!(mgr.partitions.task_state.plan.len(), 2);
}

#[test]
fn render_includes_task_state_in_state_turn_not_system() {
    let mut mgr = ContextManager::new(10_000);
    mgr.init_task("find anomalies".to_string(), vec![]);
    let rc = mgr.render();
    assert!(
        !rc.system_text.contains("[TASK STATE]"),
        "task_state must not be in system_text"
    );
    // State turn is separated from the cacheable history (turns).
    let state = rc.state_turn.as_ref().expect("should have a state turn");
    assert!(
        state
            .content
            .as_text()
            .unwrap()
            .contains("[TASK STATE] goal: find anomalies")
    );
}

#[test]
fn renewal_keeps_open_plan_steps_in_task_state() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("g".to_string(), vec![]);
    mgr.partitions.task_state.plan = vec![
        PlanStep {
            label: "done".to_string(),
            done: true,
        },
        PlanStep {
            label: "pending".to_string(),
            done: false,
        },
    ];
    mgr.renew();
    assert_eq!(mgr.partitions.task_state.open_steps(), vec!["pending"]);
}

// ── W1-1 完成态 regression gates (Step 0). RED until the planner/pure-executor rewrite. ──

#[test]
fn auto_compact_entry_logs_auto_compact_action() {
    // C regression gate: `force_compress` is the auto-compact entry point; the summary the
    // provider eventually sees (rendered from `compression_log`) must carry the **auto_compact**
    // label. The broken W1 cascade ran `compress(AutoCompact, target=0)`, so `CollapseCompactor`
    // drained the whole history first and logged `context_collapse`, then `AutoCompactor` had
    // nothing to archive — the event was labeled `auto_compact` but the log/render showed
    // `context_collapse`. The pure-executor model logs with the op's own label, restoring the
    // op-label == log-label contract end users observe (node K04/K09).
    let mut mgr = ContextManager::new(1_000);
    for i in 0..40 {
        mgr.push_history(
            CoreMessage::user(format!("turn {i}: {}", "ctx ".repeat(40))),
            200,
        );
    }
    let (saved, summary, _, _) = mgr.force_compress();
    assert!(saved > 0, "force_compress should compact a large history");
    assert!(
        summary.is_some(),
        "auto-compact summarizes the archived turns"
    );
    let actions: Vec<&str> = mgr
        .partitions
        .task_state
        .compression_log
        .iter()
        .map(|e| e.action.as_str())
        .collect();
    assert!(
        actions.last() == Some(&"auto_compact"),
        "auto-compact entry must log an auto_compact action; got {actions:?}"
    );
}

#[test]
fn skill_tool_schema_empty_when_no_skills() {
    let mgr = ContextManager::new(10_000);
    assert!(mgr.skill_tool_schema().is_none());
}

#[test]
fn skill_tool_schema_present_when_registered() {
    let mut mgr = ContextManager::new(10_000);
    mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
    assert!(
        mgr.skill_tool_schema()
            .unwrap()
            .description
            .contains("debug")
    );
}

#[test]
fn available_skills_are_reflected_in_capability_manifest() {
    let mut mgr = ContextManager::new(1_000);
    mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
    let inventory = mgr.capability_inventory();
    assert!(inventory.contains("debug"));
    assert!(inventory.contains("Debug helper"));
}

#[test]
fn toggled_meta_tools_are_reflected_in_capability_manifest() {
    let mut mgr = ContextManager::new(1_000);
    mgr.set_memory_enabled(true);
    assert!(mgr.capability_inventory().contains(MEMORY_TOOL_NAME));
    mgr.set_memory_enabled(false);
    assert!(!mgr.capability_inventory().contains(MEMORY_TOOL_NAME));
}

#[test]
fn propose_memory_schema_is_strict_and_capability_gated() {
    let mut mgr = ContextManager::new(1_000);
    assert!(mgr.propose_memory_tool_schema().is_none());
    mgr.set_propose_memory_enabled(true);
    let schema = mgr.propose_memory_tool_schema().expect("proposal capability");
    assert_eq!(schema.parameters["additionalProperties"], false);
    assert_eq!(schema.parameters["required"], serde_json::json!(["name", "kind", "content"]));
    assert!(schema.parameters["properties"].get("record_id").is_none());
}

#[test]
fn meta_tool_schemas_are_sorted() {
    let mut mgr = ContextManager::new(1_000);
    mgr.set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
    mgr.set_memory_enabled(true);
    mgr.set_propose_memory_enabled(true);
    mgr.set_knowledge_enabled(true);
    let names = mgr
        .meta_tool_schemas()
        .into_iter()
        .map(|s| s.name.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["knowledge", "memory", "note", "propose_memory", "skill"]
    );
}

#[test]
fn b1_active_skill_state_and_tool_filter() {
    let mut mgr = ContextManager::new(1_000);
    let mut debug = SkillMetadata::new("debug", "Debug helper");
    debug.allowed_tools = vec![CompactString::new("read"), CompactString::new("grep")];
    let mut review = SkillMetadata::new("review", "Reviewer");
    review.allowed_tools = vec![CompactString::new("git_diff")];
    let plain = SkillMetadata::new("plain", "No tools declared"); // empty allowed_tools
    mgr.set_available_skills(vec![debug, review, plain]);

    // No active skill ⇒ no narrowing.
    assert!(mgr.active_skill_tool_filter().is_none());

    // Activating returns the epoch-boundary changed flag.
    assert!(mgr.activate_skill("debug"));
    assert!(!mgr.activate_skill("debug")); // already active ⇒ no change

    // One restricted skill ⇒ narrow to its tools.
    let f = mgr.active_skill_tool_filter().unwrap();
    assert_eq!(f.len(), 2);
    assert!(f.contains(&CompactString::new("read")) && f.contains(&CompactString::new("grep")));

    // Second restricted skill ⇒ union (D1).
    mgr.activate_skill("review");
    let f = mgr.active_skill_tool_filter().unwrap();
    assert_eq!(f.len(), 3);
    assert!(f.contains(&CompactString::new("git_diff")));

    // An active skill with NO declared tools ⇒ unbounded ⇒ do not narrow (D3, errs-open).
    mgr.activate_skill("plain");
    assert!(mgr.active_skill_tool_filter().is_none());
}

#[test]
fn active_skill_capability_grants_follow_activation_deactivation_and_lease_expiry() {
    use crate::types::capability::{
        ActionSet, Capability, CapabilityId, ConstraintSet, Principal, ResourceSelector,
    };

    let grant = Capability {
        id: CapabilityId("read-src".into()),
        kind: CapabilityKind::Tool,
        resource: ResourceSelector("/repo/src/**".into()),
        actions: ActionSet(["read".into()].into_iter().collect()),
        constraints: ConstraintSet::default(),
        lease: None,
        delegatable: false,
        issuer: Principal("root".into()),
    };
    let mut review = SkillMetadata::new("review", "Review source files");
    review.capability_grants = vec![grant.clone()];

    let mut mgr = ContextManager::new(1_000);
    mgr.set_available_skills(vec![review]);
    assert!(mgr.active_skill_capabilities().is_empty());

    mgr.activate_skill("review");
    assert_eq!(mgr.active_skill_capabilities(), vec![grant.clone()]);

    mgr.deactivate_skill("review");
    assert!(mgr.active_skill_capabilities().is_empty());

    mgr.activate_skill_leased("review", Some(3));
    mgr.sweep_expired_skill_leases(2);
    assert_eq!(mgr.active_skill_capabilities(), vec![grant]);

    mgr.sweep_expired_skill_leases(3);
    assert!(mgr.active_skill_capabilities().is_empty());
}

#[test]
fn a_collapsed_tool_result_is_an_item_rendered_as_its_handle() {
    use crate::context::item::Fidelity;
    let mut mgr = ContextManager::new(1_000);
    for i in 0..10 {
        mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(40)), 40);
    }
    let first = mgr.partitions.history.tag(0).id;
    assert_eq!(mgr.handles.all()[0].item, first, "a handle addresses its item");
    assert_eq!(mgr.history_fidelity(0), Fidelity::Full);

    // Collapsed by provider-side clearing (`absorb_cleared_tool_results`), say.
    mgr.handles.all_mut()[0].residency = Residency::Collapsed;
    assert_eq!(mgr.history_fidelity(0), Fidelity::Handle);
    assert_eq!(mgr.history_fidelity(9), Fidelity::Full);
    let turns = mgr.render().turns;
    let stored = mgr.partitions.history.messages();
    let json = |message: &CoreMessage| serde_json::to_string(message).unwrap();
    let rendered = |call: &str| {
        turns.iter().map(json).find(|turn| turn.contains(&format!("\"call_id\":\"{call}\"")))
    };
    assert_ne!(rendered("c0"), Some(json(&stored[0])), "a collapsed item renders as its handle");
    assert_eq!(rendered("c9"), Some(json(&stored[9])), "a full item renders as stored");

    // The handle stays the item's residency when the table is restored without item ids.
    for handle in mgr.handles.all_mut() {
        handle.item = 0;
    }
    mgr.backfill_handle_items();
    assert_eq!(mgr.history_fidelity(0), Fidelity::Handle);

    mgr.reset_collapse_generation();
    assert_eq!(mgr.history_fidelity(0), Fidelity::Full);
}

fn delegating(clearing: bool, compaction: bool) -> ContextManager {
    ContextManager::with_config(
        1_000,
        ContextConfig {
            delegate_tool_result_clearing: clearing,
            delegate_compaction: compaction,
            ..ContextConfig::default()
        },
        ContextTokenEngine::char_approx(),
    )
}

/// A tier the provider performs natively is skipped, never escalated past: with compaction
/// delegated but clearing kernel-owned, the heavy tiers fall back to micro-compact.
#[test]
fn delegated_tiers_are_filtered_out_of_the_pressure_recommendation() {
    use PressureAction::*;
    let cases = [
        ((true, false), [None, None, ContextCollapse, AutoCompact]),
        ((false, true), [None, MicroCompact, MicroCompact, MicroCompact]),
        ((true, true), [None, None, None, None]),
        ((false, false), [None, MicroCompact, ContextCollapse, AutoCompact]),
    ];
    for ((clearing, compaction), expected) in cases {
        let mgr = delegating(clearing, compaction);
        let got = [None, MicroCompact, ContextCollapse, AutoCompact]
            .map(|action| mgr.kernel_owned_tier(action));
        assert_eq!(got, expected, "clearing={clearing} compaction={compaction}");
    }
    let mut mgr = delegating(true, true);
    mgr.push_history(CoreMessage::user("large history"), 975);
    assert_eq!(mgr.should_compress(), None);
    assert!(!mgr.should_renew(), "below the renewal threshold nothing happens");
    // Renewal is never delegated: it reads the full local history whatever the provider does.
    mgr.push_history(CoreMessage::user("more"), 100);
    assert!(mgr.should_renew());
}

#[test]
fn frozen_prefix_len_anchors_at_compaction_and_holds_across_appends() {
    let mut mgr = ContextManager::new(1_000);
    // Pre-compaction: no frozen region yet → providers use the rolling-pair fallback.
    for i in 0..30 {
        mgr.push_history(
            CoreMessage::user(format!("turn {i}: {}", "ctx ".repeat(30))),
            150,
        );
    }
    assert!(
        mgr.render().frozen_prefix_len.is_none(),
        "no frozen region before any compaction"
    );

    let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
    assert!(saved > 0 && !archived.is_empty(), "expected archival");

    // Immediately after compaction the hot tail is empty → deep would coincide with the tail → None.
    assert!(
        mgr.render().frozen_prefix_len.is_none(),
        "deep == tail right after compaction"
    );

    // As turns are appended, the deep boundary holds fixed while the tail grows.
    mgr.push_history(CoreMessage::user("new 1"), 5);
    let f1 = mgr
        .render()
        .frozen_prefix_len
        .expect("frozen region exists once the tail grows");
    mgr.push_history(CoreMessage::assistant("reply 1"), 5);
    mgr.push_history(CoreMessage::user("new 2"), 5);
    let rc = mgr.render();
    let f2 = rc.frozen_prefix_len.expect("frozen region holds");
    assert_eq!(
        f1, f2,
        "the deep boundary is fixed between compactions; only the tail grows"
    );
    assert!(
        f2 < rc.turns.len(),
        "deep boundary is distinct from the rolling tail"
    );
}

#[test]
fn frozen_boundary_holds_through_a_prefix_safe_compaction() {
    // P2-D × P1-E: the boundary re-anchors on a prefix-breaking compaction (cache_at = Some) but
    // is preserved through a prefix-safe one (cache_at = None) — the deep cache survives.
    let mut mgr = ContextManager::new(10_000);
    for i in 0..5 {
        mgr.push_history(CoreMessage::user(format!("m{i}")), 5);
    }
    mgr.frozen_history_len = 3; // pretend a prior compaction anchored the deep cache here

    // A no-op / prefix-safe compaction (PressureAction::None ⇒ cache_at None) must NOT move the
    // anchor — the cached [0..3] prefix is untouched, so the deep breakpoint stays put.
    let (_, _, _, cache_at) = mgr.compress(PressureAction::None);
    assert!(cache_at.is_none(), "no-op compaction is prefix-safe");
    assert_eq!(
        mgr.frozen_history_len, 3,
        "prefix-safe compaction preserves the deep-cache anchor"
    );
}

#[test]
fn collapse_generation_resets_on_autocompact() {
    let mut mgr = ContextManager::new(1_000);
    // Many oversized tool results: some will be archived by AutoCompact, the survivors
    // should come back Resident (fresh generation), not stay stuck Collapsed.
    for i in 0..20 {
        mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(120)), 60);
    }
    for handle in mgr.handles.all_mut().iter_mut().take(10) {
        handle.residency = Residency::Collapsed;
    }
    assert_eq!(
        mgr.handles.residency_for_source("c0"),
        Some(&Residency::Collapsed)
    );

    let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
    assert!(saved > 0 && !archived.is_empty(), "expected archival");

    // Every surviving tool-result handle is Resident again — the compaction boundary
    // rewrote the prefix, so the next pressure cycle re-decides from scratch.
    for h in mgr.handles.all() {
        if matches!(h.kind, HandleKind::ToolResult) {
            assert_eq!(
                h.residency,
                Residency::Resident,
                "generation reset un-collapses survivors"
            );
        }
    }
}

#[test]
fn push_history_indexes_tool_results_as_resident_handles() {
    let mut mgr = ContextManager::new(10_000);
    let msg = CoreMessage::tool(vec![ContentPart::ToolResult {
        call_id: "call_1".into(),
        output: "the tool output".to_string(),
        is_error: false,
        durable_content: None,
    }]);
    mgr.push_history(msg, 20);
    // A handle was indexed, anchored to the call_id, resident by default.
    assert_eq!(mgr.handles.all().len(), 1);
    assert_eq!(
        mgr.handles.residency_for_source("call_1"),
        Some(&Residency::Resident)
    );
    // A plain text turn allocates no handle.
    mgr.push_history(CoreMessage::user("hello"), 5);
    assert_eq!(mgr.handles.all().len(), 1);
}

// ── W1-3: handle-table GC (prune orphaned handles + bounded recompute) ──

fn tool_result_msg(call_id: &str, output: &str) -> CoreMessage {
    CoreMessage::tool(vec![ContentPart::ToolResult {
        call_id: call_id.into(),
        output: output.to_string(),
        is_error: false,
        durable_content: None,
    }])
}

#[test]
fn prune_orphaned_handles_drops_handles_whose_message_left_history() {
    let mut mgr = ContextManager::new(10_000);
    mgr.push_history(tool_result_msg("c0", "out 0"), 20);
    mgr.push_history(tool_result_msg("c1", "out 1"), 20);
    assert_eq!(mgr.handles.all().len(), 2);

    // Simulate compaction archiving the oldest tool-result message out of history.
    mgr.partitions.history.remove(0);
    mgr.prune_orphaned_handles();

    // The handle for the evicted message is gone; the live one is retained.
    assert_eq!(mgr.handles.all().len(), 1);
    assert!(mgr.handles.residency_for_source("c0").is_none());
    assert_eq!(
        mgr.handles.residency_for_source("c1"),
        Some(&Residency::Resident)
    );
}

#[test]
fn autocompact_prunes_handles_for_archived_tool_results() {
    let mut mgr = ContextManager::new(1_000);
    // Enough oversized tool results to force AutoCompact to archive some.
    for i in 0..30 {
        mgr.push_history(tool_result_msg(&format!("c{i}"), &"x".repeat(200)), 80);
    }
    assert_eq!(mgr.handles.all().len(), 30);

    let (saved, _, archived, _) = mgr.compress(PressureAction::AutoCompact);
    assert!(saved > 0 && !archived.is_empty(), "expected archival");

    // After compaction the table tracks only the tool results still in working history —
    // not the whole session. (No handle outlives its backing message.)
    let live_tool_results = mgr
        .partitions
        .history
        .messages()
        .iter()
        .filter(|m| {
            matches!(&m.content, Content::Parts(p)
            if p.iter().any(|x| matches!(x, ContentPart::ToolResult { .. })))
        })
        .count();
    assert_eq!(mgr.handles.all().len(), live_tool_results);
    assert!(
        mgr.handles.all().len() < 30,
        "table must shrink with archival"
    );
}

#[test]
fn renew_prunes_handles_for_dropped_history() {
    let mut mgr = ContextManager::new(1_000);
    mgr.init_task("g".to_string(), vec![]);
    for i in 0..20 {
        mgr.push_history(tool_result_msg(&format!("c{i}"), "data"), 60);
    }
    mgr.renew();
    // Every retained handle must still be anchored to a message present in the renewed history.
    for h in mgr.handles.all() {
        if let Some(src) = h.source.as_ref() {
            assert!(
                mgr.handles.residency_for_source(src).is_some(),
                "no dangling handle survives renewal"
            );
        }
    }
    assert!(mgr.handles.all().len() <= 20);
}

// ── K2: knowledge budget ─────────────────────────────────────────────────

#[test]
fn knowledge_budget_uses_stable_order_for_equal_value_and_warns_once() {
    // max_tokens 100 × default ratio 0.25 ⇒ budget 25. Four 10-token entries (40 used):
    // two evictable, one pinned, one skill pin.
    let mut mgr = ContextManager::new(100);
    mgr.push_knowledge(CoreMessage::system("oldest unkeyed"), 10);
    mgr.push_knowledge_entry(Some("a".into()), CoreMessage::system("keyed"), 10, false);
    mgr.push_knowledge_entry(Some("p".into()), CoreMessage::system("pinned"), 10, true);
    mgr.push_knowledge_entry(
        Some("skill:x".into()),
        CoreMessage::system("skill"),
        10,
        false,
    );

    let warn = mgr.enforce_knowledge_budget();
    assert_eq!(warn, Some((40, 25)));
    // Equal scores retain the deterministic insertion-order tie-break: unkeyed then "a".
    let e = &mgr.partitions.knowledge.entries;
    assert!(e[0].is_leaving());
    assert!(e[1].is_leaving());
    assert!(!e[2].is_leaving(), "pinned exempt");
    assert!(!e[3].is_leaving(), "skill pin exempt");

    // Warn-once per generation; marking stays idempotent.
    assert_eq!(mgr.enforce_knowledge_budget(), None);

    // The boundary sweep drops the marked entries and re-arms the warning.
    let sweep = mgr.partitions.knowledge.sweep_at_boundary();
    assert_eq!(sweep.tokens_freed, 20);
    assert_eq!(mgr.partitions.knowledge.token_count, 20);
    // Back under budget ⇒ no further warning even though it re-armed.
    assert_eq!(mgr.enforce_knowledge_budget(), None);
}

#[test]
fn knowledge_budget_warning_stands_when_only_exempt_weight_remains() {
    let mut mgr = ContextManager::new(100);
    mgr.push_knowledge_entry(
        Some("p".into()),
        CoreMessage::system("pinned heavy"),
        30,
        true,
    );
    mgr.push_knowledge_entry(
        Some("skill:x".into()),
        CoreMessage::system("skill heavy"),
        30,
        false,
    );

    // Over budget (60 > 25) but nothing evictable — warning fires, nothing marked.
    assert_eq!(mgr.enforce_knowledge_budget(), Some((60, 25)));
    assert!(
        mgr.partitions
            .knowledge
            .entries
            .iter()
            .all(|e| !e.is_leaving())
    );
}

#[test]
fn knowledge_budget_retains_old_referenced_entry_over_new_irrelevant_entry() {
    let mut mgr = ContextManager::new(100);
    mgr.push_knowledge_entry(
        Some("project:orchid".into()),
        CoreMessage::system("ORCHID uses the Atlas storage engine"),
        10,
        false,
    );
    // A committed history input is the deterministic usage fact.
    mgr.push_history(CoreMessage::user("For project:orchid keep using Atlas"), 5);
    mgr.push_knowledge_entry(
        Some("project:new".into()),
        CoreMessage::system("unrelated fresh material"),
        10,
        false,
    );
    mgr.push_knowledge_entry(
        Some("project:other".into()),
        CoreMessage::system("another unused reference"),
        10,
        false,
    );

    assert_eq!(mgr.enforce_knowledge_budget(), Some((30, 25)));
    let entries = &mgr.partitions.knowledge.entries;
    assert!(
        !entries[0].is_leaving(),
        "a real reference must raise retention"
    );
    assert!(
        entries[1].is_leaving(),
        "lowest-value entry evicts first"
    );
    assert!(
        !entries[2].is_leaving(),
        "one eviction is enough to fit"
    );
}

#[test]
fn knowledge_budget_ratio_zero_disables() {
    let mut mgr = ContextManager::new(100);
    mgr.config.knowledge_budget_ratio = 0.0;
    mgr.push_knowledge(CoreMessage::system("huge"), 90);
    assert_eq!(mgr.enforce_knowledge_budget(), None);
    assert!(!mgr.partitions.knowledge.entries[0].is_leaving());
}

#[test]
fn provider_and_output_reservations_reduce_the_hard_input_budget() {
    use crate::context::config::PromptBudgetConfig;

    let mut mgr = ContextManager::new(100);
    mgr.set_prompt_budget(PromptBudgetConfig {
        prompt_overhead_tokens: 20,
        output_reserve_tokens: 20,
        safety_margin_tokens: 10,
    });
    // spc_011-C-01: `render()`'s fixed-context accounting always recounts the system
    // partition via `engine.count()` (never trusts a stored `token_count` — see
    // `renderer.rs::render_projected`'s `system_tokens` line), so this text must overflow
    // the 50-token budget under whichever engine is configured. A single repeated character
    // (the previous `"x".repeat(240)`) reliably hit 60 under char/4 math, but real BPE
    // merges long identical-byte runs into a handful of tokens (~30, well under budget) —
    // that was calibrated to the old default, not to the overflow behavior under test.
    mgr.partitions.system.push(
        CoreMessage::system(
            "System policy directive number seven requires strict adherence. ".repeat(10),
        ),
        60,
    );

    let rendered = mgr.render();
    let overflow = rendered
        .budget_overflow
        .expect("fixed context exceeds input allowance");
    assert_eq!(overflow.max_tokens, 50);
    assert!(overflow.required_tokens > overflow.max_tokens);
}
