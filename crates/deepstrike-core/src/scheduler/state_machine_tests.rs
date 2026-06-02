//! Unit tests for [`super::LoopStateMachine`]. Extracted from state_machine.rs
//! to keep the engine logic readable; this remains a child module of
//! `state_machine` and retains access to its private items.

    use super::*;
    use crate::context::skill_catalog::SKILL_TOOL_NAME;
    use crate::types::message::Role;
    use crate::types::skill::SkillMetadata;

    fn sm() -> LoopStateMachine {
        LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            ..LoopPolicy::default()
        })
    }

    #[test]
    fn start_emits_call_llm() {
        let mut sm = sm();
        let action = sm.start(RuntimeTask::new("Say hello"));
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
    }

    #[test]
    fn resume_after_preload_runs_pending_tools_before_llm() {
        let mut sm = sm();
        sm.preload_history(vec![
            Message::user("goal"),
            Message {
                role: Role::Assistant,
                content: Content::Text("checking".into()),
                tool_calls: vec![ToolCall {
                    id: compact_str::CompactString::new("call_ping"),
                    name: compact_str::CompactString::new("ping"),
                    arguments: serde_json::json!({}),
                }],
                token_count: Some(5),
            },
        ]);
        match sm.resume_after_preload() {
            LoopAction::ExecuteTools { calls } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name.as_str(), "ping");
            }
            other => panic!("expected ExecuteTools, got {other:?}"),
        }
    }

    #[test]
    fn resume_after_preload_emits_page_in_requested_for_pending_memory() {
        let mut sm = sm();
        sm.ctx.set_memory_enabled(true);
        sm.preload_history(vec![
            Message::user("goal"),
            Message {
                role: Role::Assistant,
                content: Content::Text("recall".into()),
                tool_calls: vec![ToolCall {
                    id: compact_str::CompactString::new("mem1"),
                    name: compact_str::CompactString::new("memory"),
                    arguments: serde_json::json!({ "query": "archived", "top_k": 3 }),
                }],
                token_count: Some(5),
            },
        ]);
        let _action = sm.resume_after_preload();
        assert!(sm.observations.iter().any(|o| {
            matches!(
                o,
                LoopObservation::PageInRequested { tool, query, .. }
                    if tool == "memory" && query == "archived"
            )
        }));
    }

    #[test]
    fn resume_after_preload_emits_call_llm_without_duplicate_user() {
        let mut sm = sm();
        sm.preload_history(vec![
            Message::user("prior goal"),
            Message::assistant("partial"),
        ]);
        let history_len = sm.ctx.partitions.history.messages.len();
        let action = sm.resume_after_preload();
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert_eq!(sm.ctx.partitions.history.messages.len(), history_len);
    }

    #[test]
    fn start_places_user_message_in_history_not_signals() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("Say hello"));
        assert!(!sm.ctx.partitions.history.is_empty(), "history should have user message");
        assert!(sm.ctx.partitions.signals.is_empty(), "signals should stay empty at start");
    }

    #[test]
    fn llm_response_without_tools_terminates_and_saves_to_history() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("Say hello"));
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("Hello!"),
        });
        assert!(matches!(action, LoopAction::Done { .. }));
        assert!(sm.is_terminal());
        // Final response is committed to history
        let history = &sm.ctx.partitions.history.messages;
        assert!(
            history
                .iter()
                .any(|m| m.content.as_text() == Some("Hello!"))
        );
    }

    #[test]
    fn timeout_rolls_back() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        match sm.feed(LoopEvent::Timeout) {
            LoopAction::CallLLM { .. } => {}
            _ => panic!("expected CallLLM"),
        }
        assert!(sm.observations.iter().any(|o| {
            matches!(
                o,
                LoopObservation::Rollbacked {
                    reason: RollbackReason::Timeout,
                    ..
                }
            )
        }));
    }

    #[test]
    fn critical_signal_goes_to_signals_not_history() {
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        let history_len_before = sm.ctx.partitions.history.messages.len();

        let sig = RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::Critical, "fire");
        let action = sm.feed(LoopEvent::Signal { signal: sig });
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        assert!(sm.ctx.partitions.signals.iter().any(|s| s.contains("[INTERRUPT]")));
        assert_eq!(sm.ctx.partitions.history.messages.len(), history_len_before);
    }

    #[test]
    fn max_turns_emits_final_toolless_call_then_terminates() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            max_turns: 1,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));

        // After tool results hit maxTurns, kernel emits one final CallLLM with no tools
        let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(tools.is_empty(), "final call must have no tools")
            }
            _ => panic!("expected CallLLM for final text-only call"),
        }

        // The LLM responds with text → terminates with MaxTurns
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final summary"),
        });
        match action {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::MaxTurns);
                assert!(
                    result.final_message.is_some(),
                    "final message must be preserved"
                );
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn skill_tool_injected_in_call_llm_when_skills_registered() {
        let mut sm = sm();
        sm.ctx
            .set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        let action = sm.start(RuntimeTask::new("Fix the bug"));
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
            }
            _ => panic!("expected CallLLM"),
        }
    }

    #[test]
    fn skill_tool_not_injected_when_no_skills() {
        let mut sm = sm();
        let action = sm.start(RuntimeTask::new("Say hello"));
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(!tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
            }
            _ => panic!("expected CallLLM"),
        }
    }

    #[test]
    fn compression_emits_observation() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx
                .push_history(Message::user(format!("filler {i}")), 50);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        let obs = sm.take_observations();
        assert!(
            obs.iter()
                .any(|o| matches!(o, LoopObservation::Compressed { .. }))
        );
    }

    #[test]
    fn renewal_emits_observation_when_pressure_extreme() {
        // Renewal fires only when pressure stays > 0.98 even AFTER compression.
        // Compression only targets history + skill, so we saturate the system
        // partition (non-compressible) to keep rho above the threshold.
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        // 10 system messages × 10 tokens = 100 tokens in non-compressible partition.
        // rho = 100/100 = 1.0 > 0.98; compression on history saves nothing meaningful.
        for i in 0..10 {
            sm.ctx
                .partitions
                .system
                .push(Message::system(format!("constraint {i}")), 10);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        let obs = sm.take_observations();
        assert!(
            obs.iter()
                .any(|o| matches!(o, LoopObservation::Renewed { .. }))
        );
    }

    #[test]
    fn force_compact_emits_page_out_when_archived() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx
                .push_history(Message::user(format!("filler {i}")), 50);
        }
        assert!(sm.force_compact());
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::PageOut { .. })));
    }

    // ---- Layer 5: AutoCompact → semantic page-out (SDK does the LLM summary) ----
    //
    // Contract: AutoCompact keeps a structural summary in-context (sync, zero-I/O) and pages the
    // archived messages out to the *semantic* tier. The SDK's dream/idle pipeline LLM-summarizes
    // that tier into long-term memory (configurable LLM, default = the runtime provider). The
    // kernel never calls an LLM — it only decides WHEN/WHAT to page out.
    #[test]
    fn autocompact_pages_out_to_semantic_tier_for_llm_summary() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx.push_history(Message::user(format!("filler {i}")), 50);
        }
        assert!(sm.force_compact()); // force_compact runs an AutoCompact pass
        let obs = sm.take_observations();
        let semantic_pageout = obs.iter().any(|o| matches!(
            o,
            LoopObservation::PageOut { tier_hint, archived, action: PressureAction::AutoCompact, .. }
                if tier_hint == "semantic" && !archived.is_empty()
        ));
        assert!(
            semantic_pageout,
            "AutoCompact must page archived messages to the semantic tier for SDK LLM summary"
        );
    }

    #[test]
    fn memory_tool_proposal_emits_page_in_requested() {
        let mut sm = sm();
        sm.ctx.set_memory_enabled(true);
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("m1"),
            name: compact_str::CompactString::new("memory"),
            arguments: serde_json::json!({"query": "bugs", "top_k": 2}),
        });
        let action = sm.feed(LoopEvent::LLMResponse { message: msg });
        assert!(matches!(action, LoopAction::ExecuteTools { .. }));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::PageInRequested { tool, query, .. }
            if tool == "memory" && query == "bugs"
        )));
    }

    #[test]
    fn apply_page_in_populates_knowledge() {
        let mut sm = sm();
        sm.ctx.set_memory_enabled(true);
        sm.apply_page_in(&[crate::mm::PageInEntry {
            content: "recalled".to_string(),
            tokens: Some(3),
            source: Some("memory".to_string()),
        }]);
        assert!(!sm.ctx.partitions.knowledge.messages.is_empty());
    }

    #[test]
    fn preload_history_and_drain_new_messages() {
        let mut sm = sm();

        // Simulate restoring a prior session with one exchange
        let prior = vec![
            Message::user("Hello from last time"),
            Message::assistant("Hi! I remember."),
        ];
        sm.preload_history(prior.clone());
        assert_eq!(sm.ctx.partitions.history.messages.len(), 2);

        // Start a new turn
        sm.start(RuntimeTask::new("What did I say before?"));

        // New messages = user message from start() + (after termination) final assistant
        let new_msgs = sm.drain_new_messages();
        // At minimum the new user message must be present
        assert!(!new_msgs.is_empty());
        assert!(new_msgs.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t == "Proceed with the task described in [TASK STATE].")
                .unwrap_or(false)
        }));
        assert_eq!(sm.ctx.partitions.task_state.goal, "What did I say before?");
        // Prior session messages are NOT in drain_new_messages
        assert!(!new_msgs.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("Hello from last time"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn tool_result_content_parts_preserved_as_json() {
        use crate::types::message::Content;
        use compact_str::CompactString;

        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));

        // Simulate an LLM tool call
        let mut msg = Message::assistant("");
        msg.tool_calls.push(crate::types::message::ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("my_tool"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });

        // Feed a structured (Parts) tool result
        let structured = Content::Parts(vec![ContentPart::Text {
            text: "structured output".to_string(),
        }]);
        sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: CompactString::new("c1"),
                output: structured,
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        });

        // The history should contain a tool message with JSON-serialised content
        let tool_msgs: Vec<_> = sm
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::types::message::Role::Tool))
            .collect();
        assert!(
            !tool_msgs.is_empty(),
            "tool result message must be in history"
        );
        // Content is Parts (ToolResult part), not empty
        if let Content::Parts(parts) = &tool_msgs[0].content {
            assert!(!parts.is_empty());
        }
    }

    // ─── Milestone contract tests ──────────────────────────────────────────

    fn make_tool_schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: compact_str::CompactString::new(name),
            description: format!("tool {name}"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn milestone_contract_loads_and_reports_current_phase() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(
                crate::types::milestone::MilestonePhase::new("phase-a")
                    .with_criterion("Output contains 'hello'"),
            )
            .phase(crate::types::milestone::MilestonePhase::new("phase-b"));

        sm.load_milestone_contract(contract);
        assert_eq!(sm.current_milestone_phase_id(), Some("phase-a"));
        assert!(!sm.is_milestone_complete());
        assert_eq!(
            sm.current_milestone_criteria(),
            &["Output contains 'hello'"]
        );
    }

    #[test]
    fn milestone_pass_advances_phase_and_emits_observation() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("plan"))
            .phase(crate::types::milestone::MilestonePhase::new("implement"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("do the thing"));

        // Simulate LLM returning text-only → EvaluateMilestone
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("plan drafted"),
        });
        assert!(
            matches!(action, LoopAction::EvaluateMilestone { ref phase_id, .. } if phase_id == "plan"),
            "expected EvaluateMilestone for 'plan', got {action:?}",
        );

        // Feed a passing result
        let action2 = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("plan"),
        });
        assert!(
            matches!(action2, LoopAction::CallLLM { .. }),
            "expect CallLLM after milestone advance",
        );
        assert_eq!(sm.current_milestone_phase_id(), Some("implement"));

        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::MilestoneAdvanced { phase_id, .. } if phase_id == "plan"
        )));
    }

    #[test]
    fn milestone_fail_blocks_phase_and_emits_observation() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("plan"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("do the thing"));

        sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("bad plan"),
        });

        let action = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::fail("plan", "missing evidence"),
        });
        assert!(
            matches!(action, LoopAction::CallLLM { .. }),
            "blocked run must return CallLLM"
        );
        // Phase index must NOT advance
        assert_eq!(sm.current_milestone_phase_id(), Some("plan"));

        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::MilestoneBlocked { phase_id, reason, .. }
            if phase_id == "plan" && reason.contains("missing evidence")
        )));
    }

    #[test]
    fn milestone_unlocks_capabilities_on_advance() {
        let mut sm = sm();
        let schema = make_tool_schema("deploy_tool");
        let cap = crate::types::capability::CapabilityDescriptor::tool(schema);

        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("phase-a").unlocking(cap));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("build pipeline"));

        // Confirm tool not yet in manifest
        assert!(
            sm.ctx
                .capabilities
                .by_kind(crate::types::capability::CapabilityKind::Tool)
                .is_empty()
        );

        sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("done"),
        });
        sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("phase-a"),
        });

        // Tool must now be in the capability manifest
        let tools = sm
            .ctx
            .capabilities
            .by_kind(crate::types::capability::CapabilityKind::Tool);
        assert!(
            tools.iter().any(|c| c.id.as_str() == "deploy_tool"),
            "deploy_tool should be unlocked after phase-a passes",
        );

        // And capability_unlocked list in observation
        let obs = sm.take_observations();
        let advanced = obs.iter().find_map(|o| {
            if let LoopObservation::MilestoneAdvanced {
                capabilities_unlocked,
                ..
            } = o
            {
                Some(capabilities_unlocked)
            } else {
                None
            }
        });
        assert!(advanced.is_some(), "MilestoneAdvanced observation expected");
        assert!(advanced.unwrap().iter().any(|s| s.contains("deploy_tool")));
    }

    #[test]
    fn all_phases_complete_terminates_run() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("only-phase"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("single milestone run"));

        sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("ready"),
        });
        let done = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("only-phase"),
        });

        assert!(sm.is_milestone_complete());
        assert!(
            matches!(done, LoopAction::Done { .. }),
            "all phases done must produce Done"
        );
    }

    #[test]
    fn no_contract_terminates_normally() {
        let mut sm = sm();
        // No milestone contract loaded
        sm.start(RuntimeTask::new("simple task"));

        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("answer"),
        });
        assert!(
            matches!(action, LoopAction::Done { .. }),
            "without milestone contract, text-only response must terminate: {action:?}",
        );
    }

    #[test]
    fn mount_unmount_capability_emits_observation() {
        let mut sm = sm();
        let schema = ToolSchema {
            name: compact_str::CompactString::new("test_tool"),
            description: "test description".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let desc =
            crate::types::capability::CapabilityDescriptor::tool(schema).with_version("1.0.0");

        sm.mount_capability(desc, None, None);

        let obs = sm.take_observations();
        assert_eq!(obs.len(), 1);
        if let LoopObservation::CapabilityChanged {
            turn,
            added,
            removed,
            change_kind,
            capability_id,
            version,
            ..
        } = &obs[0]
        {
            assert_eq!(*turn, 0);
            assert_eq!(added, &vec!["Tool:test_tool".to_string()]);
            assert!(removed.is_empty());
            assert_eq!(change_kind.as_deref(), Some("mount"));
            assert_eq!(capability_id.as_deref(), Some("test_tool"));
            assert_eq!(version.as_deref(), Some("1.0.0"));
        } else {
            panic!("Expected CapabilityChanged observation");
        }

        sm.unmount_capability(crate::types::capability::CapabilityKind::Tool, "test_tool");
        let obs2 = sm.take_observations();
        assert_eq!(obs2.len(), 1);
        if let LoopObservation::CapabilityChanged {
            turn,
            added,
            removed,
            change_kind,
            capability_id,
            version,
            ..
        } = &obs2[0]
        {
            assert_eq!(*turn, 0);
            assert!(added.is_empty());
            assert_eq!(removed, &vec!["Tool:test_tool".to_string()]);
            assert_eq!(change_kind.as_deref(), Some("unmount"));
            assert_eq!(capability_id.as_deref(), Some("test_tool"));
            assert_eq!(version.as_deref(), Some("1.0.0"));
        } else {
            panic!("Expected CapabilityChanged observation");
        }
    }

    #[test]
    fn rollback_note_is_concise_by_default() {
        let reason = RollbackReason::FatalToolError {
            tool_name: "run_tests".to_string(),
            error: "exit code 1".to_string(),
        };
        let note = crate::scheduler::rollback::build_rollback_note(&reason, false);
        assert!(
            !note.contains("[SYSTEM]"),
            "default note must not contain [SYSTEM]: {note}"
        );
        assert!(
            note.contains("run_tests"),
            "note should name the tool: {note}"
        );
    }

    #[test]
    fn rollback_note_is_verbose_when_opted_in() {
        let reason = RollbackReason::Timeout;
        let note = crate::scheduler::rollback::build_rollback_note(&reason, true);
        assert!(
            note.starts_with("[SYSTEM] Transaction rollback:"),
            "verbose note must use internal format: {note}"
        );
    }

    // ─── Phase 2: suspend / resume lifecycle ─────────────────────────────────

    fn sm_with_ask_user_rule() -> LoopStateMachine {
        use crate::governance::permission::{PermissionAction, PermissionRule};
        use crate::governance::pipeline::GovernancePipeline;

        let mut sm = sm();
        let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: "sensitive.*".into(),
            action: PermissionAction::AskUser,
        });
        sm.set_governance(pipeline);
        sm
    }

    #[test]
    fn ask_user_enters_suspended_without_execute_tools() {
        let mut sm = sm_with_ask_user_rule();
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("call_a"),
            name: compact_str::CompactString::new("sensitive.read"),
            arguments: serde_json::json!({}),
        });
        let action = sm.feed(LoopEvent::LLMResponse { message: msg });
        assert!(matches!(action, LoopAction::AwaitingResume));
        assert!(sm.is_suspended());
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::Suspended { .. })));
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::ToolGated { .. })));
    }

    #[test]
    fn resume_approved_emits_execute_tools() {
        let mut sm = sm_with_ask_user_rule();
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("call_a"),
            name: compact_str::CompactString::new("sensitive.read"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });
        sm.take_observations();

        let action = sm.resume_from_suspend(vec!["call_a".to_string()], vec![]);
        match action {
            LoopAction::ExecuteTools { calls } => assert_eq!(calls.len(), 1),
            other => panic!("expected ExecuteTools, got {other:?}"),
        }
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::Resumed { .. })));
    }

    #[test]
    fn resume_all_denied_reprompts_without_execute() {
        let mut sm = sm_with_ask_user_rule();
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("call_a"),
            name: compact_str::CompactString::new("sensitive.read"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });
        sm.take_observations();

        let action = sm.resume_from_suspend(vec![], vec!["call_a".to_string()]);
        assert!(matches!(action, LoopAction::CallLLM { .. }));
    }

    #[test]
    fn spawn_sub_agent_suspends_until_completed() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("child", "child-session"),
            AgentRole::Implement,
            "child task",
        );
        let action = sm.spawn_sub_agent(spec, "parent-sess");
        assert!(matches!(action, LoopAction::AwaitingResume));
        assert!(sm.is_suspended());
        assert!(matches!(
            sm.wait_reason(),
            Some(WaitReason::SubAgentJoin(_))
        ));

        let result = SubAgentResult {
            agent_id: compact_str::CompactString::new("child"),
            result: LoopResult {
                termination: TerminationReason::Completed,
                final_message: Some(Message::assistant("ok")),
                turns_used: 1,
                total_tokens_used: 1,
            },
        };
        let resumed = sm.feed(LoopEvent::SubAgentCompleted { result });
        assert!(matches!(resumed, LoopAction::CallLLM { .. }));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::Resumed { .. })));
        assert_eq!(
            sm.agent_process("child")
                .expect("process")
                .state,
            crate::proc::ProcessState::Joined
        );
    }

    #[test]
    fn budget_exceeded_observation_on_max_turns() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            max_turns: 1,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
        assert!(matches!(action, LoopAction::CallLLM { tools, .. } if tools.is_empty()));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::BudgetExceeded { budget, .. } if budget == "max_turns"
        )));
        let done = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final"),
        });
        assert!(matches!(done, LoopAction::Done { .. }));
    }

    // ---- M1a: lifecycle-transition regression baseline ----------------------
    //
    // Pins the canonical `LoopPhase::lifecycle()` / `wait_reason()` projection
    // across real driven transitions. This is the bridge contract that M1d must
    // preserve when `LoopPhase` is split and schedulability moves onto the TCB.
    // If any of these break, the phase→TaskState mapping has drifted.

    #[test]
    fn lifecycle_idle_before_start_is_ready() {
        let sm = sm();
        // M1d: schedulability lives on the root task; before start it is `Ready` and the
        // turn-step `phase` is an inert placeholder.
        assert_eq!(sm.lifecycle(), TaskState::Ready);
        assert_eq!(sm.wait_reason(), None);
    }

    #[test]
    fn lifecycle_running_after_start() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("hi"));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        assert_eq!(sm.lifecycle(), TaskState::Running);
        assert_eq!(sm.wait_reason(), None);
    }

    #[test]
    fn lifecycle_suspended_on_ask_user_with_approval_wait() {
        let mut sm = sm_with_ask_user_rule();
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("call_a"),
            name: compact_str::CompactString::new("sensitive.read"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });
        assert!(sm.is_suspended());
        assert_eq!(sm.lifecycle(), TaskState::Suspended);
        assert_eq!(sm.wait_reason(), Some(WaitReason::Approval));
    }

    #[test]
    fn lifecycle_suspended_on_sub_agent_with_join_wait() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("child", "child-session"),
            AgentRole::Implement,
            "child task",
        );
        sm.spawn_sub_agent(spec, "parent-sess");
        assert!(sm.is_suspended());
        assert_eq!(sm.lifecycle(), TaskState::Suspended);
        assert!(matches!(
            sm.wait_reason(),
            Some(WaitReason::SubAgentJoin(_))
        ));
    }

    #[test]
    fn lifecycle_terminal_is_done_with_no_wait() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("hi"));
        let done = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final answer"),
        });
        assert!(matches!(done, LoopAction::Done { .. }));
        assert!(sm.is_terminal());
        assert!(sm.lifecycle().is_terminal());
        assert_eq!(sm.wait_reason(), None);
    }

    #[test]
    fn lifecycle_running_again_after_resume_from_suspend() {
        let mut sm = sm_with_ask_user_rule();
        sm.start(RuntimeTask::new("test"));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: compact_str::CompactString::new("call_a"),
            name: compact_str::CompactString::new("sensitive.read"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });
        assert_eq!(sm.lifecycle(), TaskState::Suspended);

        sm.resume_from_suspend(vec!["call_a".to_string()], vec![]);
        // After resume the loop is driving a turn again — back to a runnable lifecycle.
        assert_eq!(sm.lifecycle(), TaskState::Running);
        assert_eq!(sm.wait_reason(), None);
    }

    // ---- Layer 1: large tool result spool ----------------------------------

    #[test]
    fn large_tool_result_is_spooled_with_preview_and_observation() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("task"));
        sm.take_observations();

        let huge = "Z".repeat(60 * 1024); // > 50 KiB default threshold
        sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: compact_str::CompactString::new("big"),
                output: Content::Text(huge.clone()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        });

        // Kernel emitted the spool signal for the SDK to persist.
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::LargeResultSpooled { call_id, original_size, spool_ref: None, .. }
                if call_id == "big" && *original_size == (60 * 1024)
        )));

        // Context holds only the preview, not the full 60 KiB.
        let stored: usize = sm
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .filter_map(|m| match &m.content {
                Content::Parts(parts) => Some(parts),
                _ => None,
            })
            .flatten()
            .filter_map(|p| match p {
                ContentPart::ToolResult { output, .. } => Some(output.len()),
                _ => None,
            })
            .sum();
        assert!(stored < huge.len(), "spooled output should be a small preview");
        assert!(stored < 8 * 1024, "preview should be near the 2 KiB budget");
    }

    #[test]
    fn small_tool_result_is_not_spooled() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("task"));
        sm.take_observations();
        sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: compact_str::CompactString::new("ok"),
                output: Content::Text("small output".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        });
        let obs = sm.take_observations();
        assert!(!obs
            .iter()
            .any(|o| matches!(o, LoopObservation::LargeResultSpooled { .. })));
    }

    // ---- M1c: canonical TaskTable mirrors ProcessTable ----------------------

    #[test]
    fn task_table_holds_root_after_start() {
        let mut sm = sm();
        // M1d: the root task is seeded `Ready` at construction.
        assert_eq!(sm.task_table().all().len(), 1);
        assert_eq!(sm.task_table().get("root").unwrap().state, TaskState::Ready);
        sm.start(RuntimeTask::new("hi"));
        let root = sm.task_table().get("root").expect("root task");
        assert_eq!(root.state, TaskState::Running);
        assert!(root.parent.is_none());
        assert_eq!(sm.task_table().all().len(), 1);
    }

    #[test]
    fn task_table_tracks_sub_agent_lifecycle() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("child", "child-session"),
            AgentRole::Implement,
            "child task",
        );
        sm.spawn_sub_agent(spec, "parent-sess");

        // Child task registered under root, running, mirroring the process row.
        let child = sm.task_table().get("child").expect("child task");
        assert_eq!(child.state, TaskState::Running);
        assert_eq!(child.parent.as_deref(), Some("root"));
        assert_eq!(sm.task_table().children_of("root").len(), 1);
        assert_eq!(sm.task_table().all().len(), 2); // root + child

        sm.feed(LoopEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new("child"),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: Some(Message::assistant("ok")),
                    turns_used: 1,
                    total_tokens_used: 1,
                },
            },
        });

        // Join mirrored onto the task lifecycle.
        let child = sm.task_table().get("child").expect("child task");
        assert_eq!(child.state, TaskState::Done(TerminationReason::Completed));
    }
