//! Unit tests for [`super::LoopStateMachine`]. Extracted from state_machine.rs
//! to keep the engine logic readable; this remains a child module of
//! `state_machine` and retains access to its private items.

    use super::*;
    use crate::types::signal::RuntimeSignal;
    use crate::context::skill_catalog::SKILL_TOOL_NAME;
    use crate::runtime::kernel::KernelPressureAction;
    use crate::types::message::Role;
    use crate::types::skill::SkillMetadata;

    fn sm() -> LoopStateMachine {
        LoopStateMachine::new(SchedulerBudget {
            max_tokens: 128_000,
            ..SchedulerBudget::default()
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
    fn resume_after_preload_does_not_page_in_pending_memory() {
        // A live memory/knowledge tool call's result already lands in `history` via the normal
        // tool-result path once executed (decaying with the compression pyramid like any other
        // tool output). Resuming a pending call must NOT also permanently push it into `knowledge`
        // — that would make single-use retrieval content immortal, defeating the "use it, then
        // let it go" policy `knowledge` now enforces (only skills / host-pinned content belong there).
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
        let action = sm.resume_after_preload();
        assert!(matches!(action, LoopAction::ExecuteTools { .. }), "must still resume the pending call");
        // No page-in side channel fires on resume (the PageInRequested observation was
        // deleted with its retired producer); the pending call resuming is the whole contract.
        assert!(sm.observations.is_empty() || !sm.observations.iter().any(|o| {
            serde_json::to_string(o).unwrap_or_default().contains("page_in_requested")
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
                KernelObservation::Rollbacked {
                    reason: Some(RollbackReason::Timeout),
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
        let action = sm.signal_event(sig).expect("critical signal drives a turn");
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        assert!(sm.ctx.partitions.signals.iter().any(|s| s.contains("[INTERRUPT]")));
        assert_eq!(sm.ctx.partitions.history.messages.len(), history_len_before);
    }

    // ── #2-B: signal preemption ───────────────────────────────────────────────────────────────

    #[test]
    fn interrupt_now_preempts_awaited_subagent() {
        // Critical signal while the root is suspended awaiting a sub-agent → InterruptNow → preempt:
        // emit AgentPreempted, clear the SubAgentAwait, reclaim the root with a reason turn.
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("child", "child-session"),
            AgentRole::Implement,
            "child task",
        );
        sm.spawn_sub_agent(spec, "parent-sess");
        assert!(sm.is_suspended());
        sm.take_observations();

        let sig = RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::Critical, "stop and handle this");
        let action = sm.signal_event(sig);
        assert!(matches!(
            action,
            Some(LoopAction::PreemptSubAgents { ref agent_ids, .. })
                if agent_ids == &vec!["child".to_string()]
        ));
        assert!(sm.is_suspended(), "request alone does not commit preemption");
        assert!(!sm.take_observations().iter().any(|o| matches!(
            o,
            KernelObservation::AgentPreempted { .. }
        )));

        let action = sm.resolve_preempt();
        assert!(matches!(action, LoopAction::CallLLM { .. }), "result reclaims the root");
        assert!(!sm.is_suspended(), "confirmed preemption cleared SubAgentAwait");
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::AgentPreempted { agent_ids, .. } if agent_ids == &vec!["child".to_string()]
        )), "AgentPreempted names the aborted child");
    }

    #[test]
    fn interrupt_now_aborts_owning_workflow() {
        // Critical signal while a workflow's node is running → tear the whole WorkflowRun down
        // (§6.1a): emit WorkflowCompleted (non-completed nodes failed) + AgentPreempted, clear it.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(RuntimeTask::new("root node"), AgentRole::Implement)]);
        let action = sm.load_workflow(spec, "sess");
        if let LoopAction::SpawnWorkflow { nodes, .. } = action {
            sm.resolve_workflow_spawn(
                nodes.into_iter().map(|node| node.agent_id).collect(),
                Vec::new(),
            );
        }
        assert!(sm.workflow_active());
        sm.take_observations();

        let sig = RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::Critical, "abort");
        let action = sm.signal_event(sig);
        assert!(matches!(action, Some(LoopAction::PreemptSubAgents { .. })));
        assert!(sm.workflow_active(), "request does not tear down workflow");
        assert!(!sm.take_observations().iter().any(|o| matches!(
            o,
            KernelObservation::AgentPreempted { .. }
        )));

        sm.resolve_preempt();
        assert!(!sm.workflow_active(), "InterruptNow tore the workflow down");
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::AgentPreempted { .. })));
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowCompleted { failed, .. } if failed.contains(&"wf-node0".to_string())
        )), "the running node is reported failed on abort");
    }

    #[test]
    fn high_urgency_interrupt_does_not_preempt() {
        // High (not Critical) signal while busy → soft Interrupt: record the directive, do NOT abort
        // the running sub-agent or force a turn. Distinguishes Interrupt from InterruptNow.
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("child", "child-session"),
            AgentRole::Implement,
            "child task",
        );
        sm.spawn_sub_agent(spec, "parent-sess");
        sm.take_observations();

        let sig = RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::High, "fyi handle soon");
        let action = sm.signal_event(sig);
        assert!(action.is_none(), "soft Interrupt does not force a turn");
        assert!(sm.is_suspended(), "running sub-agent is NOT aborted");
        let obs = sm.take_observations();
        assert!(!obs.iter().any(|o| matches!(o, KernelObservation::AgentPreempted { .. })), "no preemption");
        assert!(sm.ctx.partitions.task_state.directives.iter().any(|d| d.contains("handle soon")), "directive recorded for next boundary");
    }

    #[test]
    fn user_directive_survives_renewal() {
        // Part B: a mid-task user command (arriving as an acted-on signal) is promoted into the
        // durable directive channel and must survive a sprint renewal — unlike the ephemeral signal
        // copy, which renewal clears. This is the fix for "latest command loses salience across
        // consecutive contexts".
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("ship the feature"));

        let sig = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "do NOT modify the migration files",
        );
        sm.signal_event(sig).expect("critical signal drives a turn");
        assert!(
            sm.ctx.partitions.task_state.directives.iter().any(|d| d.contains("migration files")),
            "acted-on signal is promoted to a durable directive"
        );

        // Force renewal: saturate the non-compressible system partition so rho stays > 0.98.
        for i in 0..10 {
            sm.ctx.partitions.system.push(Message::system(format!("c{i}")), 10);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        assert!(
            sm.take_observations().iter().any(|o| matches!(o, KernelObservation::Renewed { .. })),
            "renewal fired"
        );

        // Ephemeral signal copy is gone, but the durable directive survives and renders.
        assert!(
            sm.ctx.partitions.task_state.directives.iter().any(|d| d.contains("migration files")),
            "user directive must survive a sprint renewal"
        );
        assert!(sm.ctx.partitions.task_state.format_compact().contains("migration files"));
    }

    #[test]
    fn max_turns_emits_final_toolless_call_then_terminates() {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 128_000,
            max_turns: 1,
            ..SchedulerBudget::default()
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

    /// P1-B B2: once a skill with declared `allowed_tools` is active, `emit_call_llm` exposes only
    /// `meta-tools ∪ stable-core ∪ allowed_tools`. Meta-tools stay (D5); undeclared/inactive = full.
    #[test]
    fn active_skill_gates_exposed_tools_with_stable_core() {
        let mut sm = sm();
        sm.tools = vec![
            make_tool_schema("read"),
            make_tool_schema("write"),
            make_tool_schema("bash"),
            make_tool_schema("grep"),
        ];
        // A skill that declares only {read, grep}; stable-core keeps {bash}; `write` is gated out.
        let mut debug = SkillMetadata::new("debug", "Debug helper");
        debug.allowed_tools = vec![compact_str::CompactString::new("read"), compact_str::CompactString::new("grep")];
        sm.ctx.set_available_skills(vec![debug]);
        sm.ctx.set_stable_core_tools([compact_str::CompactString::new("bash")]);

        // Before activation: all tools exposed (no narrowing).
        match sm.start(RuntimeTask::new("go")) {
            LoopAction::CallLLM { tools, .. } => {
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(["read", "write", "bash", "grep"].iter().all(|n| names.contains(n)));
                // skill meta-tool present (skills registered) and must survive gating later.
                assert!(names.contains(&SKILL_TOOL_NAME));
            }
            _ => panic!("expected CallLLM"),
        }

        // Activate the skill → next emit narrows.
        sm.ctx.activate_skill("debug");
        match sm.emit_call_llm() {
            LoopAction::CallLLM { tools, .. } => {
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(names.contains(&"read") && names.contains(&"grep"), "declared: {names:?}");
                assert!(names.contains(&"bash"), "stable-core kept: {names:?}");
                assert!(names.contains(&SKILL_TOOL_NAME), "meta-tool exempt: {names:?}");
                assert!(!names.contains(&"write"), "undeclared gated out: {names:?}");
            }
            _ => panic!("expected CallLLM"),
        }
    }

    /// P1-B B4: the gated toolset is byte-stable *within* an epoch (no active-set change ⇒ identical
    /// tools every turn — the cache prefix never churns mid-epoch) and changes only when the active
    /// set changes (the single epoch boundary where D re-anchors the cache).
    #[test]
    fn gating_is_byte_stable_within_an_epoch() {
        fn names(action: LoopAction) -> Vec<String> {
            match action {
                LoopAction::CallLLM { tools, .. } => {
                    let mut n: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
                    n.sort();
                    n
                }
                _ => panic!("expected CallLLM"),
            }
        }
        let mut sm = sm();
        sm.tools = vec![make_tool_schema("read"), make_tool_schema("write"), make_tool_schema("grep")];
        let mut debug = SkillMetadata::new("debug", "d");
        debug.allowed_tools = vec![compact_str::CompactString::new("read")];
        let mut review = SkillMetadata::new("review", "r");
        review.allowed_tools = vec![compact_str::CompactString::new("grep")];
        sm.ctx.set_available_skills(vec![debug, review]);

        sm.start(RuntimeTask::new("go"));
        sm.ctx.activate_skill("debug");
        let n1 = names(sm.emit_call_llm());
        let n2 = names(sm.emit_call_llm()); // no activation change → identical
        assert_eq!(n1, n2, "toolset must be byte-stable within an epoch");
        assert!(n1.contains(&"read".to_string()) && !n1.contains(&"write".to_string()));

        sm.ctx.activate_skill("review"); // epoch boundary
        let n3 = names(sm.emit_call_llm());
        assert_ne!(n1, n3, "activating another skill changes the toolset");
        assert!(n3.contains(&"grep".to_string()), "union now includes review's tools: {n3:?}");
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

    /// P0-A (tool gating): a top-level run carrying an `AgentRunSpec` with a
    /// capability filter must only expose the allow-listed tools each turn — the
    /// static per-run profile. The filter is an allow-list (empty = allow all),
    /// applied in `emit_call_llm`, and is byte-stable across the run, so it never
    /// busts the cache prefix. Gates the same path sub-agents already use.
    #[test]
    fn top_level_run_capability_filter_gates_exposed_tools() {
        use crate::types::agent::{
            AgentCapabilityFilter, AgentIdentity, AgentRole, AgentRunSpec,
        };
        let mut sm = sm();
        sm.tools = vec![
            make_tool_schema("read"),
            make_tool_schema("write"),
            make_tool_schema("bash"),
            make_tool_schema("search"),
        ];
        // Static profile: only `read` + `search` are exposed for this run.
        let mut spec = AgentRunSpec::new(
            AgentIdentity::new("root", "root-session"),
            AgentRole::Custom,
            "do the task",
        );
        spec.capability_filter = AgentCapabilityFilter {
            allowed_kinds: Vec::new(),
            allowed_ids: vec![
                compact_str::CompactString::new("read"),
                compact_str::CompactString::new("search"),
            ],
        };
        sm.run_spec = Some(spec);

        match sm.start(RuntimeTask::new("do the task")) {
            LoopAction::CallLLM { tools, .. } => {
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(names.contains(&"read"), "read should be exposed: {names:?}");
                assert!(names.contains(&"search"), "search should be exposed: {names:?}");
                assert!(!names.contains(&"write"), "write must be gated out: {names:?}");
                assert!(!names.contains(&"bash"), "bash must be gated out: {names:?}");
            }
            _ => panic!("expected CallLLM"),
        }
    }

    /// Counterpart: with no run spec (the default top-level run), every base tool
    /// is exposed — i.e. gating is strictly opt-in (铁律: no config = old behavior).
    #[test]
    fn top_level_run_without_spec_exposes_all_tools() {
        let mut sm = sm();
        sm.tools = vec![
            make_tool_schema("read"),
            make_tool_schema("write"),
            make_tool_schema("bash"),
        ];
        match sm.start(RuntimeTask::new("do the task")) {
            LoopAction::CallLLM { tools, .. } => {
                let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                assert!(names.contains(&"read") && names.contains(&"write") && names.contains(&"bash"));
            }
            _ => panic!("expected CallLLM"),
        }
    }

    #[test]
    fn compression_emits_observation() {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
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
                .any(|o| matches!(o, KernelObservation::Compressed { .. }))
        );
    }

    #[test]
    fn renewal_emits_observation_when_pressure_extreme() {
        // Renewal fires only when pressure stays > 0.98 even AFTER compression.
        // Compression only targets history + skill, so we saturate the system
        // partition (non-compressible) to keep rho above the threshold.
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
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
                .any(|o| matches!(o, KernelObservation::Renewed { .. }))
        );
    }

    #[test]
    fn force_compact_emits_page_out_when_archived() {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx
                .push_history(Message::user(format!("filler {i}")), 50);
        }
        assert!(sm.force_compact());
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::Compressed { .. })));
        let action = sm.externalize_pending_host_effect(LoopAction::AwaitingResume);
        assert!(matches!(action, LoopAction::ArchivePageOut { archived, tier, .. } if !archived.is_empty() && tier == "semantic"));
    }

    #[test]
    fn knowledge_remove_sweeps_at_compaction_boundary_and_observes() {
        // K1: a RemoveKnowledge mark leaves the entry rendered (system[1] bytes untouched) until
        // a compaction boundary, where the sweep drops it and a KnowledgeSwept observation fires.
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        sm.ctx.push_knowledge_entry(Some("ref".into()), Message::system("REFDOC"), 5, false);
        sm.ctx.remove_knowledge("ref");
        assert!(sm.ctx.render().system_knowledge.contains("REFDOC"), "still rendered pre-boundary");

        for i in 0..10 {
            sm.ctx.push_history(Message::user(format!("filler {i}")), 50);
        }
        assert!(sm.force_compact());
        assert!(!sm.ctx.render().system_knowledge.contains("REFDOC"), "swept at the boundary");
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::KnowledgeSwept { removed_keys, .. } if removed_keys.contains(&"ref".to_string())
        )));
    }

    #[test]
    fn knowledge_upsert_applies_at_renewal_boundary() {
        // K1: renewal is a boundary too — a staged same-key upsert lands there, and entry
        // identity (the key) survives the renewal's wholesale knowledge carry-over.
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        sm.ctx.push_knowledge_entry(Some("ref".into()), Message::system("V1"), 5, false);
        sm.ctx.push_knowledge_entry(Some("ref".into()), Message::system("V2"), 5, false);
        assert!(sm.ctx.render().system_knowledge.contains("V1"));

        sm.ctx.renew();
        let rendered = sm.ctx.render().system_knowledge;
        assert!(rendered.contains("V2"), "upsert applied at renewal");
        assert!(!rendered.contains("V1"));
        assert_eq!(sm.ctx.partitions.knowledge.len(), 1, "keyed identity carried, not duplicated");
    }

    // ---- Reactive recovery ladder (lifted from the SDK runners into the kernel) ----

    fn compactible_machine() -> LoopStateMachine {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx.push_history(Message::user(format!("filler {i}")), 50);
        }
        sm
    }

    #[test]
    fn recover_overflow_compacts_and_retries() {
        let mut sm = compactible_machine();
        let action = sm.recover_from_provider_error("HTTP 413: prompt is too long");
        // Recovered headroom ⇒ retry the provider with a freshly compacted context.
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert_eq!(sm.recovery_attempts, 1);
        // The eviction rode out as observations so the SDK still archives the evicted messages.
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::Compressed { .. })));
    }

    #[test]
    fn recover_overflow_exhausted_terminates_context_overflow() {
        // Fresh machine with nothing compactible ⇒ force_compact saves 0 ⇒ honest terminal.
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        let action = sm.recover_from_provider_error("context_length_exceeded");
        match action {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::ContextOverflow);
            }
            other => panic!("expected Done(ContextOverflow), got {other:?}"),
        }
    }

    #[test]
    fn recover_non_overflow_terminates_error() {
        let mut sm = compactible_machine();
        let action = sm.recover_from_provider_error("500 Internal Server Error");
        match action {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::Error);
            }
            other => panic!("expected Done(Error), got {other:?}"),
        }
    }

    #[test]
    fn recover_attempt_cap_is_bounded() {
        // Even a provider that 413s forever must terminate, not loop. Drive past the cap and
        // assert a terminal appears within a bounded number of attempts.
        let mut sm = compactible_machine();
        let mut terminated = false;
        for _ in 0..6 {
            if let LoopAction::Done { result } = sm.recover_from_provider_error("413 too long") {
                assert_eq!(result.termination, TerminationReason::ContextOverflow);
                terminated = true;
                break;
            }
        }
        assert!(terminated, "recovery ladder must terminate under repeated overflow");
    }

    #[test]
    fn recovery_attempts_reset_on_successful_response() {
        let mut sm = compactible_machine();
        let action = sm.recover_from_provider_error("413 too long");
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert_eq!(sm.recovery_attempts, 1);
        // A response that fits resets the ladder (mirrors the per-turn SDK guard reset).
        sm.feed(LoopEvent::LLMResponse { message: Message::assistant("recovered") });
        assert_eq!(sm.recovery_attempts, 0);
    }

    #[test]
    fn is_prompt_too_long_classifier() {
        use super::eviction::is_prompt_too_long;
        assert!(is_prompt_too_long("Error 413"));
        assert!(is_prompt_too_long("prompt is TOO LONG"));
        assert!(is_prompt_too_long("context_length_exceeded"));
        assert!(!is_prompt_too_long("429 rate limited"));
        assert!(!is_prompt_too_long("connection reset"));
    }

    // ---- Max-output-tokens recovery (Phase 4) ----

    #[test]
    fn truncated_response_continues_then_resets() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("write a long thing"));
        // Cut off at the output cap with no tool call ⇒ keep the partial and re-call (don't finish).
        sm.set_pending_stop_reason(Some("max_tokens".into()));
        let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("partial...") });
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert_eq!(sm.output_recovery_attempts, 1);
        // A clean finish (no stop_reason) terminates normally AND resets the ladder.
        let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("...the rest. done") });
        match action {
            LoopAction::Done { result } => assert_eq!(result.termination, TerminationReason::Completed),
            other => panic!("expected Done(Completed), got {other:?}"),
        }
        assert_eq!(sm.output_recovery_attempts, 0);
    }

    #[test]
    fn truncation_recovery_is_bounded() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("write forever"));
        // 3 attempts continue; the 4th consecutive truncation gives up and accepts the partial.
        for _ in 0..3 {
            sm.set_pending_stop_reason(Some("length".into()));
            let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("more") });
            assert!(matches!(action, LoopAction::CallLLM { .. }));
        }
        sm.set_pending_stop_reason(Some("length".into()));
        let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("more") });
        match action {
            LoopAction::Done { result } => assert_eq!(result.termination, TerminationReason::Completed),
            other => panic!("expected Done(Completed) after the cap, got {other:?}"),
        }
    }

    #[test]
    fn no_stop_reason_terminates_normally() {
        // The no-op safety path: a provider that never reports stop_reason (every non-Anthropic
        // provider today) finishes the turn as before — no spurious continue.
        let mut sm = sm();
        sm.start(RuntimeTask::new("answer"));
        let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("the answer") });
        assert!(matches!(action, LoopAction::Done { .. }));
        assert_eq!(sm.output_recovery_attempts, 0);
    }

    // ---- Layer 5: AutoCompact → semantic page-out (SDK does the LLM summary) ----
    //
    // Contract: AutoCompact keeps a structural summary in-context (sync, zero-I/O) and pages the
    // archived messages out to the *semantic* tier. The SDK's dream/idle pipeline LLM-summarizes
    // that tier into long-term memory (configurable LLM, default = the runtime provider). The
    // kernel never calls an LLM — it only decides WHEN/WHAT to page out.
    #[test]
    fn autocompact_pages_out_to_semantic_tier_for_llm_summary() {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 100,
            max_turns: 100,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx.push_history(Message::user(format!("filler {i}")), 50);
        }
        assert!(sm.force_compact()); // force_compact runs an AutoCompact pass
        let semantic_pageout = matches!(
            sm.externalize_pending_host_effect(LoopAction::AwaitingResume),
            LoopAction::ArchivePageOut { tier, archived, action: KernelPressureAction::AutoCompact, .. }
                if tier == "semantic" && !archived.is_empty()
        );
        assert!(
            semantic_pageout,
            "AutoCompact must hint the semantic tier for the archived batch (SDK LLM summary)"
        );
    }

    #[test]
    fn memory_tool_proposal_does_not_page_in() {
        // A proposed `memory`/`knowledge` tool call is executed normally (ExecuteTools) and its
        // eventual result lands in `history` like any other tool result — it must NOT also trigger
        // a `PageInRequested`/permanent-knowledge side channel (removed: that made single-use
        // retrievals immortal). `knowledge` is reserved for skills / host-pinned content now.
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
        assert!(!obs.iter().any(|o| {
            serde_json::to_string(o).unwrap_or_default().contains("page_in_requested")
        }));
    }

    #[test]
    fn apply_page_in_still_populates_knowledge_for_stable_pins() {
        // `apply_page_in` itself remains a valid mechanism for genuinely durable content (a host
        // explicitly pinning reference material, or the SDK pushing loaded skill text) — only the
        // automatic per-tool-call producer was retired, not the sink.
        let mut sm = sm();
        sm.apply_page_in(&[crate::mm::PageInEntry {
            content: "skill: debugging playbook".to_string(),
            tokens: Some(3),
            source: Some("skill".to_string()),
            key: None,
            pinned: false,
        }]);
        assert!(!sm.ctx.partitions.knowledge.is_empty());
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
            KernelObservation::MilestoneAdvanced { phase_id, .. } if phase_id == "plan"
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
            KernelObservation::MilestoneBlocked { phase_id, reason, .. }
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
            if let KernelObservation::MilestoneAdvanced {
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
        if let KernelObservation::CapabilityChanged {
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
        if let KernelObservation::CapabilityChanged {
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
        assert!(matches!(action, LoopAction::RequestApproval { .. }));
        assert!(sm.is_suspended());
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::Suspended { .. })));
        assert!(!obs.iter().any(|o| matches!(o, KernelObservation::ToolGated { .. })));
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

        let action = sm.resolve_approval(vec!["call_a".to_string()], vec![]);
        match action {
            LoopAction::ExecuteTools { calls } => assert_eq!(calls.len(), 1),
            other => panic!("expected ExecuteTools, got {other:?}"),
        }
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::Resumed { .. })));
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

        let action = sm.resolve_approval(vec![], vec!["call_a".to_string()]);
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
                loop_continue: None,
                classify_branch: None,
                tournament_winner: None,
                pace_decision: None,
            },
        };
        let resumed = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result });
        assert!(matches!(resumed, LoopAction::CallLLM { .. }));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, KernelObservation::Resumed { .. })));
        assert_eq!(
            sm.agent_process("child")
                .expect("process")
                .state,
            crate::proc::ProcessState::Joined
        );
    }

    #[test]
    fn budget_exceeded_observation_on_max_turns() {
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 128_000,
            max_turns: 1,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
        assert!(matches!(action, LoopAction::CallLLM { tools, .. } if tools.is_empty()));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::BudgetExceeded { budget, .. } if budget == "max_turns"
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
    // If any of these break, the phase→TaskLifecycle mapping has drifted.

    #[test]
    fn lifecycle_idle_before_start_is_ready() {
        let sm = sm();
        // M1d: schedulability lives on the root task; before start it is `Ready` and the
        // turn-step `phase` is an inert placeholder.
        assert_eq!(sm.lifecycle(), TaskLifecycle::Ready);
        assert_eq!(sm.wait_reason(), None);
    }

    #[test]
    fn lifecycle_running_after_start() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("hi"));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        assert_eq!(sm.lifecycle(), TaskLifecycle::Running);
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
        assert_eq!(sm.lifecycle(), TaskLifecycle::Suspended);
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
        assert_eq!(sm.lifecycle(), TaskLifecycle::Suspended);
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
        assert_eq!(sm.lifecycle(), TaskLifecycle::Suspended);

        sm.resolve_approval(vec!["call_a".to_string()], vec![]);
        // After resume the loop is driving a turn again — back to a runnable lifecycle.
        assert_eq!(sm.lifecycle(), TaskLifecycle::Running);
        assert_eq!(sm.wait_reason(), None);
    }

    // ---- Phase 0: budget-axis termination baseline -------------------------
    //
    // Pins the SM-level termination for all three budget axes (only `max_turns`
    // was covered before). P1c swaps the `should_terminate` call site for the
    // pure `schedule()`; these assert the resulting `BudgetExceeded` reason and
    // terminal `TerminationReason` stay byte-identical across that swap.

    #[test]
    fn budget_exceeded_observation_on_token_budget() {
        use crate::types::result::TerminationReason;
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 128_000,
            max_total_tokens: 10,
            ..SchedulerBudget::default()
        });
        sm.start(RuntimeTask::new("test"));
        sm.take_observations();
        // A tool result whose token_count pushes cumulative usage over the budget.
        let action = sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: compact_str::CompactString::new("c"),
                output: Content::Text("x".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: Some(20),
            }],
        });
        assert!(matches!(action, LoopAction::CallLLM { tools, .. } if tools.is_empty()));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::BudgetExceeded { budget, .. } if budget == "token_budget"
        )));
        let done = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final"),
        });
        assert!(matches!(
            done,
            LoopAction::Done { result } if result.termination == TerminationReason::TokenBudget
        ));
    }

    #[test]
    fn budget_exceeded_observation_on_wall_time() {
        use crate::types::result::TerminationReason;
        let mut sm = LoopStateMachine::new(SchedulerBudget {
            max_tokens: 128_000,
            max_wall_ms: Some(1_000),
            ..SchedulerBudget::default()
        });
        sm.set_observed_time(0); // anchors started_at_ms
        sm.start(RuntimeTask::new("test"));
        sm.take_observations();
        sm.set_observed_time(2_000); // 2s elapsed >= 1s wall budget
        let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
        assert!(matches!(action, LoopAction::CallLLM { tools, .. } if tools.is_empty()));
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            KernelObservation::BudgetExceeded { budget, .. } if budget == "wall_time"
        )));
        let done = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final"),
        });
        assert!(matches!(
            done,
            LoopAction::Done { result } if result.termination == TerminationReason::Timeout
        ));
    }

    // ---- Phase 0: AgentProcess view-shape baseline -------------------------
    //
    // Pins the exact `AgentProcess` field values exposed via `agent_process(es)`
    // across the spawn→join lifecycle. P1b makes `ProcessTable` a derived view
    // over the `TaskTable`; the reconstructed `AgentProcess` must reproduce these
    // fields byte-for-byte so session-log / os-snapshot goldens stay stable.

    #[test]
    fn agent_process_view_shape_is_pinned_across_spawn_and_join() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        use crate::proc::ProcessState;
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

        let p = sm.agent_process("child").expect("process after spawn");
        assert_eq!(p.agent_id.as_str(), "child");
        assert_eq!(p.parent_session_id.as_str(), "parent-sess");
        assert_eq!(p.role, AgentRole::Implement);
        assert_eq!(p.state, ProcessState::Running);
        assert!(p.result.is_none());
        // Snapshot the spawn-time shape for cross-check after the view migration.
        let isolation_at_spawn = p.isolation;
        let inheritance_at_spawn = p.context_inheritance;

        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new("child"),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: Some(Message::assistant("done")),
                    turns_used: 2,
                    total_tokens_used: 42,
                    loop_continue: None,
                    classify_branch: None,
                    tournament_winner: None,
                    pace_decision: None,
                },
            },
        });

        let p = sm.agent_process("child").expect("process after join");
        assert_eq!(p.state, ProcessState::Joined);
        assert_eq!(p.isolation, isolation_at_spawn);
        assert_eq!(p.context_inheritance, inheritance_at_spawn);
        let result = p.result.as_ref().expect("join result");
        assert_eq!(result.result.termination, TerminationReason::Completed);
        assert_eq!(result.result.turns_used, 2);
        assert_eq!(result.result.total_tokens_used, 42);
        assert_eq!(sm.agent_processes().len(), 1);
    }

    // ---- Phase 2 (M2): resource quotas at the syscall trap -----------------

    #[test]
    fn spawn_quota_denies_beyond_concurrency_limit() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_concurrent_subagents: Some(1),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // First spawn is allowed; the loop suspends awaiting the join.
        let a1 = sm.spawn_sub_agent(
            AgentRunSpec::new(AgentIdentity::sub_agent("a", "a-sess"), AgentRole::Implement, "t"),
            "parent-sess",
        );
        assert!(matches!(a1, LoopAction::AwaitingResume));
        assert_eq!(sm.task_table().children_of("root").len(), 1);
        sm.take_observations();

        // Second spawn while one is still Running: denied by quota → rolled back, no new child.
        let a2 = sm.spawn_sub_agent(
            AgentRunSpec::new(AgentIdentity::sub_agent("b", "b-sess"), AgentRole::Implement, "t"),
            "parent-sess",
        );
        assert!(matches!(a2, LoopAction::CallLLM { .. }));
        assert_eq!(sm.task_table().children_of("root").len(), 1);
        assert!(sm.agent_process("b").is_none());
    }

    #[test]
    fn spawn_quota_denies_when_depth_exceeds_limit() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_spawn_depth: Some(0), // no sub-agents permitted (direct children are depth 1)
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let action = sm.spawn_sub_agent(
            AgentRunSpec::new(AgentIdentity::sub_agent("c", "c-sess"), AgentRole::Implement, "t"),
            "parent-sess",
        );
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(!sm.is_suspended());
        assert!(sm.agent_process("c").is_none());
    }

    #[test]
    fn no_quota_leaves_spawn_unconditionally_allowed() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        // Pre-M2 behavior: without set_resource_quota, spawns are never quota-denied.
        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();
        let action = sm.spawn_sub_agent(
            AgentRunSpec::new(AgentIdentity::sub_agent("d", "d-sess"), AgentRole::Implement, "t"),
            "parent-sess",
        );
        assert!(matches!(action, LoopAction::AwaitingResume));
        assert!(sm.is_suspended());
    }

    #[test]
    fn memory_write_quota_rate_limits_within_window() {
        use crate::mm::memory::{MemoryMetadata, MemoryWriteRequest};
        use crate::syscall::{Disposition, Syscall};

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            memory_writes_per_window: Some((2, 60_000)), // 2 writes / 60s
            ..Default::default()
        });
        sm.set_observed_time(1_000);
        let req = MemoryWriteRequest {
            metadata: MemoryMetadata { name: "m".into(), description: "d".into(), ..Default::default() },
            content: "c".to_string(),
        };

        assert!(sm.gate_syscall(&Syscall::WriteMemory(req.clone())).is_allowed());
        assert!(sm.gate_syscall(&Syscall::WriteMemory(req.clone())).is_allowed());
        // Third write within the window is rate-limited.
        assert!(matches!(
            sm.gate_syscall(&Syscall::WriteMemory(req.clone())),
            Disposition::RateLimited { .. }
        ));
        // After the window elapses, writes are allowed again.
        sm.set_observed_time(1_000 + 60_000);
        assert!(sm.gate_syscall(&Syscall::WriteMemory(req)).is_allowed());
    }

    // ---- Layer 1: large tool result spool ----------------------------------

    #[test]
    fn large_tool_result_emits_spool_effect_and_keeps_preview() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("task"));
        sm.take_observations();

        let huge = "Z".repeat(60 * 1024); // > 50 KiB default threshold
        let continuation = sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: compact_str::CompactString::new("big"),
                output: Content::Text(huge.clone()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        });

        let action = sm.externalize_pending_host_effect(continuation);
        assert!(matches!(
            action,
            LoopAction::SpoolLargeResult { call_id, output, original_size, .. }
                if call_id == "big" && output == huge && original_size == (60 * 1024)
        ));

        // No success fact exists until the host returns the correlated result.
        let obs = sm.take_observations();
        assert!(!obs.iter().any(|o| matches!(o, KernelObservation::LargeResultSpooled { .. })));

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
            .any(|o| matches!(o, KernelObservation::LargeResultSpooled { .. })));
    }

    // ---- M1c: canonical TaskTable mirrors ProcessTable ----------------------

    #[test]
    fn task_table_holds_root_after_start() {
        let mut sm = sm();
        // M1d: the root task is seeded `Ready` at construction.
        assert_eq!(sm.task_table().all().len(), 1);
        assert_eq!(sm.task_table().get("root").unwrap().state, TaskLifecycle::Ready);
        sm.start(RuntimeTask::new("hi"));
        let root = sm.task_table().get("root").expect("root task");
        assert_eq!(root.state, TaskLifecycle::Running);
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
        assert_eq!(child.state, TaskLifecycle::Running);
        assert_eq!(child.parent.as_deref(), Some("root"));
        assert_eq!(sm.task_table().children_of("root").len(), 1);
        assert_eq!(sm.task_table().all().len(), 2); // root + child

        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new("child"),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: Some(Message::assistant("ok")),
                    turns_used: 1,
                    total_tokens_used: 1,
                    loop_continue: None,
                    classify_branch: None,
                    tournament_winner: None,
                    pace_decision: None,
                },
            },
        });

        // Join mirrored onto the task lifecycle.
        let child = sm.task_table().get("child").expect("child task");
        assert_eq!(child.state, TaskLifecycle::Done(TerminationReason::Completed));
    }

    // ---- W0: kernel-resident workflow executor -----------------------------

    fn wf_completed(agent_id: &str) -> crate::types::result::SubAgentResult {
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};
        SubAgentResult {
            agent_id: compact_str::CompactString::new(agent_id),
            result: LoopResult {
                termination: TerminationReason::Completed,
                final_message: Some(Message::assistant("ok")),
                turns_used: 1,
                total_tokens_used: 1,
                loop_continue: None,
                classify_branch: None,
                tournament_winner: None,
                pace_decision: None,
            },
        }
    }

    /// A workflow completion that signals the loop should stop early (v2 "until done").
    fn wf_completed_stop(agent_id: &str) -> crate::types::result::SubAgentResult {
        let mut r = wf_completed(agent_id);
        r.result.loop_continue = Some(false);
        r
    }

    /// A classifier completion that selects a branch label.
    fn wf_completed_branch(agent_id: &str, label: &str) -> crate::types::result::SubAgentResult {
        let mut r = wf_completed(agent_id);
        r.result.classify_branch = Some(label.to_string());
        r
    }

    /// A tournament judge completion reporting its winning entrant id.
    fn wf_completed_winner(agent_id: &str, winner: &str) -> crate::types::result::SubAgentResult {
        let mut r = wf_completed(agent_id);
        r.result.tournament_winner = Some(winner.to_string());
        r
    }

    /// The spawn descriptors from the most recent `WorkflowBatchSpawned` observation.
    fn last_batch_spawns(
        obs: &[KernelObservation],
    ) -> Vec<crate::orchestration::workflow::WorkflowSpawnInfo> {
        obs.iter()
            .rev()
            .find_map(|o| match o {
                KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn count_spawned(obs: &[KernelObservation]) -> usize {
        obs.iter()
            .filter(|o| matches!(o, KernelObservation::AgentProcessChanged { state, .. } if state == "running"))
            .count()
    }

    fn accept_workflow_spawn(sm: &mut LoopStateMachine, action: LoopAction) -> LoopAction {
        match action {
            LoopAction::SpawnWorkflow { nodes, .. } => sm.resolve_workflow_spawn(
                nodes.into_iter().map(|node| node.agent_id).collect(),
                Vec::new(),
            ),
            other => other,
        }
    }

    fn load_workflow_started(
        sm: &mut LoopStateMachine,
        spec: crate::orchestration::workflow::WorkflowSpec,
        session_id: &str,
    ) -> LoopAction {
        let action = sm.load_workflow(spec, session_id);
        accept_workflow_spawn(sm, action)
    }

    fn feed_workflow(sm: &mut LoopStateMachine, event: LoopEvent) -> LoopAction {
        let action = sm.feed(event);
        accept_workflow_spawn(sm, action)
    }

    fn submit_workflow_started(
        sm: &mut LoopStateMachine,
        spec: crate::orchestration::workflow::WorkflowSpec,
        session_id: &str,
        submitter_agent_id: Option<&str>,
    ) -> LoopAction {
        let action = sm.submit_workflow(spec, session_id, submitter_agent_id);
        accept_workflow_spawn(sm, action)
    }

    fn submit_workflow_nodes_started(
        sm: &mut LoopStateMachine,
        nodes: Vec<crate::orchestration::workflow::WorkflowNode>,
        submitter_agent_id: Option<&str>,
    ) -> LoopAction {
        let action = sm.submit_workflow_nodes(nodes, submitter_agent_id);
        accept_workflow_spawn(sm, action)
    }

    #[test]
    fn workflow_fanout_spawns_batch_then_synthesizes() {
        use crate::orchestration::workflow::fanout_synthesize;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = fanout_synthesize(
            vec![
                RuntimeTask::new("w0"),
                RuntimeTask::new("w1"),
                RuntimeTask::new("w2"),
            ],
            RuntimeTask::new("synth"),
        );
        let action = load_workflow_started(&mut sm, spec, "sess");
        assert!(matches!(action, LoopAction::AwaitingResume));
        assert!(sm.workflow_active());
        assert!(sm.is_suspended());

        // First batch: the 3 workers spawn in parallel, one Suspended barrier over all of them.
        let obs = sm.take_observations();
        assert_eq!(count_spawned(&obs), 3);
        let suspended = obs.iter().find_map(|o| match o {
            KernelObservation::Suspended {
                reason,
                pending_calls,
                ..
            } => Some((reason.clone(), pending_calls.len())),
            _ => None,
        });
        assert_eq!(suspended, Some(("workflow_batch".to_string(), 3)));

        // Two workers done → still suspended (batch not drained).
        assert!(matches!(
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
                result: wf_completed("wf-node0")
            }),
            LoopAction::AwaitingResume
        ));
        assert!(matches!(
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
                result: wf_completed("wf-node1")
            }),
            LoopAction::AwaitingResume
        ));
        assert!(sm.workflow_active());
        sm.take_observations();

        // Third worker done → batch drains, synth (wf-node3) becomes the next gated batch.
        assert!(matches!(
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
                result: wf_completed("wf-node2")
            }),
            LoopAction::AwaitingResume
        ));
        assert!(sm.workflow_active());
        let obs = sm.take_observations();
        assert_eq!(count_spawned(&obs), 1); // synth spawned
        assert!(sm.agent_process("wf-node3").is_some());

        // Synth done → no more ready nodes → workflow finishes, parent resumes.
        let final_action = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: wf_completed("wf-node3"),
        });
        assert!(matches!(final_action, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
        assert!(
            sm.take_observations()
                .iter()
                .any(|o| matches!(o, KernelObservation::Resumed { .. }))
        );
    }

    #[test]
    fn workflow_linear_chain_spawns_one_at_a_time() {
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // A → B → C
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("A"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("B"), AgentRole::Implement).with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("C"), AgentRole::Implement).with_depends_on(vec![1]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // only A

        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: wf_completed("wf-node0"),
        });
        assert_eq!(count_spawned(&sm.take_observations()), 1); // B
        assert!(sm.workflow_active());

        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: wf_completed("wf-node1"),
        });
        assert_eq!(count_spawned(&sm.take_observations()), 1); // C

        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: wf_completed("wf-node2"),
        });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn workflow_node_concurrency_limit_serializes_via_defer() {
        // W2-1 收口: under the run-queue executor a concurrency cap no longer *fails* the surplus
        // node (the old batch barrier `mark_denied`'d it, starving its dependents). Instead the cap
        // is transient backpressure: the surplus node is DEFERRED and runs once a slot frees, so the
        // whole DAG still completes — just serialized.
        use crate::orchestration::workflow::fanout_synthesize;

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_concurrent_subagents: Some(1),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // 2 workers → synth. Cap 1: only wf-node0 spawns now; wf-node1 is deferred (NOT failed).
        let spec = fanout_synthesize(
            vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")],
            RuntimeTask::new("synth"),
        );
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // only wf-node0 fits the slot
        assert!(sm.agent_process("wf-node0").is_some());
        assert!(sm.agent_process("wf-node1").is_none()); // deferred, not yet spawned

        // wf-node0 completes → its slot frees → the deferred wf-node1 now spawns (not starved).
        assert!(matches!(
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") }),
            LoopAction::AwaitingResume
        ));
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node1 spawned after the slot freed
        assert!(sm.agent_process("wf-node1").is_some());
        assert!(sm.workflow_active());

        // wf-node1 completes → both workers done → synth (wf-node2) becomes ready and spawns.
        assert!(matches!(
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") }),
            LoopAction::AwaitingResume
        ));
        assert_eq!(count_spawned(&sm.take_observations()), 1); // synth
        assert!(sm.agent_process("wf-node2").is_some());

        // synth completes → DAG done, parent resumes. Every node ran despite the cap of 1.
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node2") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn submit_workflow_nodes_appends_and_spawns_mid_run() {
        // R3-1: a running workflow node submits more work; the appended node spawns (gated) at once
        // and the workflow stays alive until it too completes — the kernel side of true
        // loop-until-done / dynamic fan-out, with no new gate or observation.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // A single root node spawns first.
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node0
        assert!(sm.agent_process("wf-node0").is_some());

        // While wf-node0 runs, submit one more node — it spawns immediately as wf-node1.
        let action = submit_workflow_nodes_started(&mut sm,
            vec![WorkflowNode::new(
                RuntimeTask::new("discovered-work"),
                AgentRole::Implement,
            )],
            None,
        );
        assert!(matches!(action, LoopAction::AwaitingResume));
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node1
        assert!(sm.agent_process("wf-node1").is_some());
        assert!(sm.workflow_active());

        // Empty submission (and a submission with no active workflow) is a no-op.
        submit_workflow_nodes_started(&mut sm, vec![], None);
        assert_eq!(count_spawned(&sm.take_observations()), 0);

        // The root completes but the workflow keeps running its submitted node.
        assert!(matches!(
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") }),
            LoopAction::AwaitingResume
        ));
        assert!(sm.workflow_active(), "still running the submitted node");

        // The submitted node completes → DAG done, parent resumes.
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn submit_workflow_nodes_denied_past_max_workflow_nodes_quota() {
        // R3-1 governance: a submission that would grow the DAG past max_workflow_nodes is denied
        // (runaway loop-until-done backstop); the workflow continues with its existing nodes.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_workflow_nodes: Some(1),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node0

        // Submitting would grow the DAG to 2 > max(1) → denied; no wf-node1 spawns.
        submit_workflow_nodes_started(&mut sm,
            vec![WorkflowNode::new(RuntimeTask::new("more"), AgentRole::Implement)],
            None,
        );
        assert_eq!(count_spawned(&sm.take_observations()), 0);
        assert!(sm.agent_process("wf-node1").is_none(), "denied submission does not spawn");

        // The workflow finishes with just the root.
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn submit_workflow_bootstraps_dag_when_no_workflow_active() {
        // M5/G1: a top-level agent (no workflow active) authors a spec → the kernel *bootstraps* the
        // DAG in this same kernel and spawns its first batch. This is the article's headline capability.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();
        assert!(!sm.workflow_active(), "no workflow before authoring");

        // Two independent nodes → both spawn in the bootstrap batch.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement),
        ]);
        let action = submit_workflow_started(&mut sm, spec, "sess", None);
        assert!(matches!(action, LoopAction::AwaitingResume { .. }));
        assert!(sm.workflow_active(), "spec bootstrapped the DAG");
        assert_eq!(count_spawned(&sm.take_observations()), 2); // wf-node0 + wf-node1

        // Both complete → the agent-authored workflow finishes.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") });
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn submit_workflow_flattens_onto_active_workflow() {
        // M5/G1: a second authoring while a workflow is active *flattens* (appends) — never stacks a
        // child workflow. Proves bootstrap-or-flatten: one DAG, one kernel, no recursion of kernels.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node0

        // Author a second spec while wf-node0 runs → its nodes flatten onto the live DAG as wf-node1.
        let more = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("more"),
            AgentRole::Implement,
        )]);
        submit_workflow_started(&mut sm, more, "sess", None);
        assert_eq!(count_spawned(&sm.take_observations()), 1); // wf-node1 appended, not a new workflow
        assert!(sm.agent_process("wf-node1").is_some(), "flattened node spawned in the same DAG");

        // The DAG finishes only after both nodes complete (2 total) — confirming a single workflow.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") });
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn submit_workflow_denied_past_max_workflow_nodes_quota() {
        // M5/G1 governance: an authored spec that would overgrow the DAG is denied by the same
        // max_workflow_nodes backstop as SubmitNodes — bootstrap is refused, no workflow installed.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_workflow_nodes: Some(2),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // A 3-node spec > max(2) → denied; nothing spawns and no workflow is installed.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("c"), AgentRole::Implement),
        ]);
        let action = submit_workflow_started(&mut sm, spec, "sess", None);
        assert!(matches!(action, LoopAction::AwaitingResume { .. }));
        assert_eq!(count_spawned(&sm.take_observations()), 0);
        assert!(!sm.workflow_active(), "denied authoring installs no workflow");
    }

    #[test]
    fn submit_workflow_bootstrap_announces_batch_with_submitter() {
        // W-3/W-N3: the bootstrap arm emits WorkflowNodesSubmitted{base:0, submitter} exactly like
        // the flatten arm — so the SDK can persist an agent-authored workflow's nodes and resume
        // them (the host never had this spec), and resume can drop batches whose submitter re-runs.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("a"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("b"), AgentRole::Implement),
        ]);
        submit_workflow_started(&mut sm, spec, "sess", Some("author-agent"));
        let obs = sm.take_observations();
        let submitted = obs
            .iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowNodesSubmitted { base, count, submitter, .. } => {
                    Some((*base, *count, submitter.clone()))
                }
                _ => None,
            })
            .expect("bootstrap announces its batch");
        assert_eq!(submitted, (0, 2, Some("author-agent".to_string())));

        // Flatten while active: the appended batch is announced at its real base with the submitter.
        let more = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("c"),
            AgentRole::Implement,
        )]);
        submit_workflow_started(&mut sm, more, "sess", Some("wf-node0"));
        let obs = sm.take_observations();
        let flattened = obs
            .iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowNodesSubmitted { base, count, submitter, .. } => {
                    Some((*base, *count, submitter.clone()))
                }
                _ => None,
            })
            .expect("flatten announces its batch");
        assert_eq!(flattened, (2, 1, Some("wf-node0".to_string())));
    }

    #[test]
    fn zero_concurrency_quota_denies_workflow_spawns_instead_of_stalling() {
        // W-6: max_concurrent_subagents=0 can never free a slot; Defer would park every node
        // forever and the drive loop would emit an empty "completed" outcome. Nodes must FAIL.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_concurrent_subagents: Some(0),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("a"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");
        let obs = sm.take_observations();
        assert_eq!(count_spawned(&obs), 0, "nothing spawns under a zero-slot pool");
        let completed = obs.iter().find_map(|o| match o {
            KernelObservation::WorkflowCompleted { completed, failed, .. } => {
                Some((completed.clone(), failed.clone()))
            }
            _ => None,
        });
        if let Some((completed, failed)) = completed {
            assert!(completed.is_empty());
            assert_eq!(failed, vec!["wf-node0".to_string()], "node denied, not silently skipped");
        }
    }

    #[test]
    fn workflow_batch_spawned_carries_remaining_budget_under_quota() {
        // G4 budget-as-signal: a coordinator node reads remaining headroom to scale its submission.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.set_resource_quota(crate::governance::quota::ResourceQuota {
            max_workflow_nodes: Some(5),
            max_concurrent_subagents: Some(3),
            ..Default::default()
        });
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");

        // The first batch (wf-node0) reports: 1/5 nodes used → 4 remaining; 1 running → 2 slots left.
        let budget = sm
            .take_observations()
            .into_iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowBatchSpawned { budget, .. } => budget,
                _ => None,
            })
            .expect("budget present under an active quota");
        assert_eq!(budget.nodes_used, 1);
        assert_eq!(budget.nodes_max, Some(5));
        assert_eq!(budget.nodes_remaining, Some(4));
        assert_eq!(budget.running_subagents, 1);
        assert_eq!(budget.max_concurrent_subagents, Some(3));
        assert_eq!(budget.concurrency_remaining, Some(2));
        // M4/G5 token headroom: no tokens spent at load → used 0, remaining == the full cap.
        assert_eq!(budget.tokens_used, 0);
        assert!(budget.tokens_max.is_some());
        assert_eq!(budget.tokens_remaining, budget.tokens_max);
    }

    #[test]
    fn workflow_batch_spawned_omits_budget_without_quota() {
        // No resource quota installed → nothing to bound → no budget signal (additive: omitted).
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        load_workflow_started(&mut sm, spec, "sess");
        let had_budget = sm.take_observations().into_iter().any(|o| {
            matches!(o, KernelObservation::WorkflowBatchSpawned { budget: Some(_), .. })
        });
        assert!(!had_budget, "no quota ⇒ no budget signal");
    }

    #[test]
    fn quarantined_node_output_is_labeled_crossing_into_trusted_context() {
        // R3-3: the kernel marks a quarantined node's output as untrusted-origin when it crosses into
        // the trusted parent context. Shaping the content into a summary stays SDK-side; the kernel
        // enforces the provenance label so a trusted consumer can't mistake it for trusted content.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        // A single quarantined node — Explore defaults to ReadOnly, so the quarantine invariant holds.
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read-untrusted"), AgentRole::Explore).quarantined(),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        sm.take_observations();
        assert!(sm.agent_process("wf-node0").is_some(), "quarantined ReadOnly node spawns");

        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") });
        assert!(
            sm.ctx
                .partitions
                .signals
                .iter()
                .any(|s| s.contains("[quarantined sub-agent wf-node0]")),
            "quarantined output is labeled untrusted-origin on crossing: {:?}",
            sm.ctx.partitions.signals
        );
    }

    #[test]
    fn quarantined_node_with_write_isolation_is_denied_in_kernel() {
        // Part A #3: the kernel enforces the quarantine invariant — a quarantined node (reads
        // untrusted content) that declares a write-capable isolation is denied at spawn, starving
        // its dependents, rather than trusting the SDK to honor read-only.
        use crate::orchestration::workflow::{NodeTrust, WorkflowNode, WorkflowSpec};
        use crate::types::agent::{AgentIsolation, AgentRole};

        let mut sm = sm();
        sm.start(RuntimeTask::new("triage untrusted input"));
        sm.take_observations();

        // node0: quarantined but asks for Shared (write) isolation → must be denied.
        // node1: depends on node0 → starves (never becomes ready).
        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read untrusted webpage"), AgentRole::Explore)
                .with_isolation(AgentIsolation::Shared)
                .with_trust(NodeTrust::Quarantined),
            WorkflowNode::new(RuntimeTask::new("act on it"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        let action = load_workflow_started(&mut sm, spec, "sess");

        // Nothing spawns (node0 denied, node1 starved) → workflow finishes immediately.
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(sm.agent_process("wf-node0").is_none(), "quarantined+write node denied");
        assert!(sm.agent_process("wf-node1").is_none(), "dependent starves");
        assert!(!sm.workflow_active());
        let obs = sm.take_observations();
        assert!(
            obs.iter().any(|o| matches!(o, KernelObservation::Rollbacked { .. }))
                || sm.ctx.partitions.signals.iter().any(|s| s.to_lowercase().contains("quarantine")),
            "quarantine denial is surfaced"
        );
    }

    #[test]
    fn loop_node_reruns_then_unblocks_dependent_via_drive_workflow() {
        // A#2 Loop node end-to-end through the run-queue executor: a Loop{2} node runs two
        // iterations (distinct agent ids wf-node0-i0/i1), and its dependent spawns only after the
        // loop finishes. Proves runtime node addition with zero new ABI (same WorkflowBatchSpawned/
        // SubAgentCompleted contract).
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("iterate to convergence"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("refine"), AgentRole::Implement).with_loop(2),
            WorkflowNode::new(RuntimeTask::new("finalize"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");

        // Iteration 0 spawns.
        assert_eq!(count_spawned(&sm.take_observations()), 1);
        assert!(sm.agent_process("wf-node0-i0").is_some());
        assert!(sm.agent_process("wf-node1").is_none(), "dependent waits for the loop");

        // i0 completes → loop continues → iteration 1 spawns (not the dependent).
        assert!(matches!(
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0-i0") }),
            LoopAction::AwaitingResume
        ));
        assert_eq!(count_spawned(&sm.take_observations()), 1);
        assert!(sm.agent_process("wf-node0-i1").is_some(), "second iteration spawned");
        assert!(sm.agent_process("wf-node1").is_none());

        // i1 completes → loop done (max_iters=2) → the dependent finally spawns.
        assert!(matches!(
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0-i1") }),
            LoopAction::AwaitingResume
        ));
        assert_eq!(count_spawned(&sm.take_observations()), 1);
        assert!(sm.agent_process("wf-node1").is_some(), "dependent spawns after the loop ends");

        // dependent completes → workflow finishes, parent resumes.
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn loop_node_stops_early_on_loop_continue_false() {
        // A#2 v2 "until done": a Loop{5} node normally runs 5 iterations, but an iteration that
        // reports loop_continue=Some(false) ends the loop early and promotes the dependent.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("search until no new findings"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("probe"), AgentRole::Explore).with_loop(5),
            WorkflowNode::new(RuntimeTask::new("report"), AgentRole::Plan).with_depends_on(vec![0]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // i0

        // i0 → continue (no opinion); i1 spawns.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0-i0") });
        assert_eq!(count_spawned(&sm.take_observations()), 1); // i1
        assert!(sm.agent_process("wf-node1").is_none(), "dependent still waiting");

        // i1 signals "done" (loop_continue=false) at iteration 2 of 5 → loop ends early; dependent spawns.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed_stop("wf-node0-i1") });
        assert_eq!(count_spawned(&sm.take_observations()), 1);
        assert!(sm.agent_process("wf-node1").is_some(), "early stop promoted the dependent");
        assert!(sm.agent_process("wf-node0-i2").is_none(), "no third iteration ran");

        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn classify_node_routes_to_chosen_branch_and_prunes_others() {
        // A#2 Classify: a classifier node's result selects one branch to run; the other branches'
        // nodes are pruned (never spawn). Conditional edges in an otherwise static DAG.
        use crate::orchestration::workflow::{ClassifyBranch, WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("triage this ticket"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("classify ticket"), AgentRole::Plan).with_classify(vec![
                ClassifyBranch { label: "bug".into(), nodes: vec![1] },
                ClassifyBranch { label: "feature".into(), nodes: vec![2] },
            ]),
            WorkflowNode::new(RuntimeTask::new("fix the bug"), AgentRole::Implement)
                .with_depends_on(vec![0]),
            WorkflowNode::new(RuntimeTask::new("build the feature"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1); // the classifier (wf-node0)

        // Classifier returns "bug" → the bug branch (node 1) runs; the feature branch (node 2) is pruned.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed_branch("wf-node0", "bug") });
        assert_eq!(count_spawned(&sm.take_observations()), 1);
        assert!(sm.agent_process("wf-node1").is_some(), "chosen branch runs");
        assert!(sm.agent_process("wf-node2").is_none(), "other branch pruned");

        // bug branch completes → workflow done (feature branch was pruned).
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn tournament_node_drives_bracket_end_to_end_via_drive_workflow() {
        // A#2 Tournament: a controller node spawns no agent of its own — it fans out into 4 entrant
        // generators, then a single-elimination bracket of pairwise judges (each carrying its
        // JudgeMatch over the existing WorkflowBatchSpawned contract), and resolves to one champion
        // before its dependent runs. Exercises the real run-queue executor end-to-end.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("pick the strongest candidate"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("which ad converts best?"), AgentRole::Plan)
                .with_tournament(vec![
                    RuntimeTask::new("draft ad A"),
                    RuntimeTask::new("draft ad B"),
                    RuntimeTask::new("draft ad C"),
                    RuntimeTask::new("draft ad D"),
                ]),
            WorkflowNode::new(RuntimeTask::new("ship the winner"), AgentRole::Implement)
                .with_depends_on(vec![0]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");

        // Node 0 = controller, node 1 = the gated dependent ("ship the winner"). Entrant children are
        // appended after the static spec → wf-node2..5; the controller spawns no agent of its own.
        let obs = sm.take_observations();
        assert_eq!(count_spawned(&obs), 4, "four entrant generators");
        assert!(sm.agent_process("wf-node0").is_none(), "controller spawns no agent of its own");
        for id in ["wf-node2", "wf-node3", "wf-node4", "wf-node5"] {
            assert!(sm.agent_process(id).is_some(), "{id} entrant spawned");
        }
        assert!(last_batch_spawns(&obs).iter().all(|n| n.judge_match.is_none()), "entrants aren't judges");

        // Entrants finish → round-1 judges spawn (2 matches), each with its pair as a JudgeMatch.
        for id in ["wf-node2", "wf-node3", "wf-node4"] {
            feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed(id) });
            assert_eq!(count_spawned(&sm.take_observations()), 0, "no judges until every entrant is in");
        }
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node5") });
        let r1 = sm.take_observations();
        assert_eq!(count_spawned(&r1), 2, "two round-1 judges (wf-node6, wf-node7)");
        let r1_matches: Vec<_> =
            last_batch_spawns(&r1).iter().filter_map(|n| n.judge_match.clone()).collect();
        assert_eq!(r1_matches.len(), 2, "each round-1 judge carries a match");
        assert_eq!(r1_matches[0].left, "wf-node2");
        assert_eq!(r1_matches[0].right, "wf-node3");
        assert_eq!(r1_matches[1].left, "wf-node4");
        assert_eq!(r1_matches[1].right, "wf-node5");
        assert!(sm.agent_process("wf-node1").is_none(), "dependent waits for the whole bracket");

        // Judges report winners (entrant 2 and entrant 4 advance) → one final judge spawns.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed_winner("wf-node6", "wf-node2") });
        assert_eq!(count_spawned(&sm.take_observations()), 0, "final waits for both round-1 judges");
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed_winner("wf-node7", "wf-node4") });
        let r2 = sm.take_observations();
        assert_eq!(count_spawned(&r2), 1, "one final judge (wf-node8)");
        let r2_match = last_batch_spawns(&r2)[0].judge_match.clone().expect("final judge match");
        assert_eq!(r2_match.left, "wf-node2");
        assert_eq!(r2_match.right, "wf-node4");

        // Final judge crowns entrant 4 → controller completes; only now does the dependent run.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed_winner("wf-node8", "wf-node4") });
        assert_eq!(count_spawned(&sm.take_observations()), 1, "dependent spawns after the bracket");
        assert!(sm.agent_process("wf-node1").is_some(), "the 'ship the winner' dependent");

        // Dependent completes → workflow finishes, parent loop resumes.
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node1") });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }

    #[test]
    fn quarantined_node_read_only_is_allowed() {
        // The invariant only bites on write-capable isolation — a quarantined read-only node runs.
        use crate::orchestration::workflow::{NodeTrust, WorkflowNode, WorkflowSpec};
        use crate::types::agent::{AgentIsolation, AgentRole};

        let mut sm = sm();
        sm.start(RuntimeTask::new("triage"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("read untrusted webpage"), AgentRole::Explore)
                .with_isolation(AgentIsolation::ReadOnly)
                .with_trust(NodeTrust::Quarantined),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        assert_eq!(count_spawned(&sm.take_observations()), 1, "read-only quarantined node spawns");
        assert!(sm.agent_process("wf-node0").is_some());
    }

    #[test]
    fn workflow_run_queue_unblocks_dependents_per_node() {
        // W2-1 收口 — the batch barrier is gone: a node whose dependency finishes early starts
        // immediately, without waiting for the slowest sibling in that dependency's layer.
        // DAG:  A(0) ─┬─► C(2)   and   B(1) ─► (only C)         plus  A(0) ─► D(3)
        // i.e. C depends on A & B; D depends only on A. When A completes (B still running), D must
        // spawn right away — the old batch path would have waited for B too.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();

        let spec = WorkflowSpec::new(vec![
            WorkflowNode::new(RuntimeTask::new("A"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("B"), AgentRole::Implement),
            WorkflowNode::new(RuntimeTask::new("C"), AgentRole::Implement).with_depends_on(vec![0, 1]),
            WorkflowNode::new(RuntimeTask::new("D"), AgentRole::Implement).with_depends_on(vec![0]),
        ]);
        load_workflow_started(&mut sm, spec, "sess");
        // A and B have no deps → both spawn in the first round.
        assert_eq!(count_spawned(&sm.take_observations()), 2);

        // A completes while B is still running → D (depends only on A) spawns immediately; C still
        // waits on B. This is the per-node unblock the run queue delivers.
        feed_workflow(&mut sm, LoopEvent::SubAgentCompleted { result: wf_completed("wf-node0") });
        let obs = sm.take_observations();
        assert_eq!(count_spawned(&obs), 1, "D unblocks on A alone, not waiting for B");
        assert!(sm.agent_process("wf-node3").is_some(), "D (node 3) is running");
        assert!(sm.agent_process("wf-node2").is_none(), "C still waits on B");
        assert!(sm.workflow_active());
    }

    #[test]
    fn single_spawn_path_leaves_workflow_inactive() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut sm = sm();
        sm.start(RuntimeTask::new("parent"));
        sm.take_observations();
        sm.spawn_sub_agent(
            AgentRunSpec::new(
                AgentIdentity::sub_agent("child", "child-session"),
                AgentRole::Implement,
                "child task",
            ),
            "parent-sess",
        );
        assert!(!sm.workflow_active());
        let done = feed_workflow(&mut sm, LoopEvent::SubAgentCompleted {
            result: wf_completed("child"),
        });
        assert!(matches!(done, LoopAction::CallLLM { .. }));
        assert!(!sm.workflow_active());
    }


    // ── O6: RepeatFuse — the hard rungs above the 2c soft STOP ────────────────────────────────

    /// An assistant turn proposing exactly one tool call.
    fn fuse_tool_turn(name: &str, args: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: Content::Text("".into()),
            tool_calls: vec![ToolCall {
                id: compact_str::CompactString::new("c1"),
                name: compact_str::CompactString::new(name),
                arguments: args,
            }],
            token_count: Some(3),
        }
    }

    fn fuse_tool_result() -> ToolResult {
        ToolResult {
            call_id: compact_str::CompactString::new("c1"),
            output: Content::Text("unchanged".into()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: Some(2),
        }
    }

    /// Drive one full identical tool turn; returns the action from the LLMResponse feed.
    fn fuse_drive_turn(sm: &mut LoopStateMachine, args: serde_json::Value) -> LoopAction {
        let action = sm.feed(LoopEvent::LLMResponse { message: fuse_tool_turn("set_title", args) });
        if matches!(action, LoopAction::ExecuteTools { .. }) {
            return sm.feed(LoopEvent::ToolResults { results: vec![fuse_tool_result()] });
        }
        action
    }

    #[test]
    fn repeat_fuse_denies_at_threshold_and_feeds_directive_back() {
        let mut sm = sm();
        sm.set_repeat_fuse(crate::governance::repeat_fuse::RepeatFuseConfig {
            enabled: true,
            deny_after: 3,
            terminate_after: 0, // deny rung only
        });
        sm.start(RuntimeTask::new("set the title"));

        // Turns 1–2: identical call executes normally (streak 1, 2 < deny_after).
        for _ in 0..2 {
            let a = fuse_drive_turn(&mut sm, serde_json::json!({"title": "same"}));
            assert!(matches!(a, LoopAction::CallLLM { .. }), "turn should complete and re-call");
            assert!(
                !sm.observations.iter().any(|o| matches!(o, KernelObservation::RepeatFuseTripped { .. })),
                "fuse must not trip below the threshold"
            );
        }

        // Turn 3: streak hits deny_after ⇒ turn rolled back, directive note in signals.
        let a = sm.feed(LoopEvent::LLMResponse {
            message: fuse_tool_turn("set_title", serde_json::json!({"title": "same"})),
        });
        assert!(matches!(a, LoopAction::CallLLM { .. }), "deny re-calls with the note, got {a:?}");
        assert!(sm.observations.iter().any(|o| matches!(
            o,
            KernelObservation::RepeatFuseTripped { action, count, .. } if action == "deny" && *count == 3
        )));
        assert!(
            sm.ctx.partitions.signals.iter().any(|s| s.contains("repeat fuse")),
            "directive note must reach the model: {:?}",
            sm.ctx.partitions.signals
        );
        assert!(!sm.is_terminal(), "deny rung must not terminate the run");
    }

    #[test]
    fn repeat_fuse_ignores_same_tool_with_different_args() {
        let mut sm = sm();
        sm.set_repeat_fuse(crate::governance::repeat_fuse::RepeatFuseConfig {
            enabled: true,
            deny_after: 3,
            terminate_after: 5,
        });
        sm.start(RuntimeTask::new("process items"));

        // A legit loop: same tool, DIFFERENT args every turn — never trips any rung.
        for i in 0..6 {
            let a = fuse_drive_turn(&mut sm, serde_json::json!({"n": i}));
            assert!(matches!(a, LoopAction::CallLLM { .. }));
        }
        assert!(
            !sm.observations.iter().any(|o| matches!(o, KernelObservation::RepeatFuseTripped { .. })),
            "args-varying iteration is progress, not a stall"
        );
    }

    #[test]
    fn repeat_fuse_escalates_to_no_progress_termination() {
        let mut sm = sm();
        sm.set_repeat_fuse(crate::governance::repeat_fuse::RepeatFuseConfig {
            enabled: true,
            deny_after: 2,
            terminate_after: 3,
        });
        sm.start(RuntimeTask::new("set the title"));

        // Turn 1 executes; turn 2 hits the deny rung; turn 3 (model ignores the note and
        // re-issues the same call) hits the terminate rung.
        let _ = fuse_drive_turn(&mut sm, serde_json::json!({"title": "same"}));
        let _ = sm.feed(LoopEvent::LLMResponse {
            message: fuse_tool_turn("set_title", serde_json::json!({"title": "same"})),
        });
        let a = sm.feed(LoopEvent::LLMResponse {
            message: fuse_tool_turn("set_title", serde_json::json!({"title": "same"})),
        });
        // Terminate rung: one final no-tools report turn…
        match a {
            LoopAction::CallLLM { tools, .. } => assert!(tools.is_empty(), "final turn must strip tools"),
            other => panic!("expected final report CallLLM, got {other:?}"),
        }
        assert!(sm.observations.iter().any(|o| matches!(
            o,
            KernelObservation::RepeatFuseTripped { action, count, .. } if action == "terminate" && *count == 3
        )));
        // …then the run terminates NoProgress on the model's text response.
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("stuck: title already set") });
        match done {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::NoProgress);
            }
            other => panic!("expected Done(NoProgress), got {other:?}"),
        }
    }

    #[test]
    fn repeat_fuse_disabled_lets_identical_calls_run() {
        let mut sm = sm();
        sm.set_repeat_fuse(crate::governance::repeat_fuse::RepeatFuseConfig {
            enabled: false,
            deny_after: 2,
            terminate_after: 3,
        });
        sm.start(RuntimeTask::new("poll status"));
        for _ in 0..5 {
            let a = fuse_drive_turn(&mut sm, serde_json::json!({"id": "x"}));
            assert!(matches!(a, LoopAction::CallLLM { .. }));
        }
        assert!(
            !sm.observations.iter().any(|o| matches!(o, KernelObservation::RepeatFuseTripped { .. }))
        );
    }

    // ── O4: turn-end criteria gate (the Stop-hook analog) ─────────────────────────────────────

    #[test]
    fn criteria_gate_injects_one_self_check_before_completed() {
        let mut sm = sm();
        let mut task = RuntimeTask::new("ship the fix");
        task.criteria = vec!["tests pass".into(), "docs updated".into()];
        sm.start(task);

        // First finish attempt: gate fires — one more turn, with the check in signals.
        let a = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("done, I think") });
        assert!(matches!(a, LoopAction::CallLLM { .. }), "gate must re-call, got {a:?}");
        assert!(!sm.is_terminal());
        assert!(sm.observations.iter().any(|o| matches!(o, KernelObservation::CriteriaGateFired { .. })));
        assert!(
            sm.ctx.partitions.signals.iter().any(|s| s.contains("[CRITERIA CHECK]") && s.contains("tests pass")),
            "self-check must land in signals: {:?}",
            sm.ctx.partitions.signals
        );

        // Second finish attempt: gate already fired — run completes normally.
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("verified: all criteria met") });
        match done {
            LoopAction::Done { result } => assert_eq!(result.termination, TerminationReason::Completed),
            other => panic!("expected Done(Completed), got {other:?}"),
        }
    }

    #[test]
    fn criteria_gate_is_a_noop_without_criteria() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("say hello"));
        let a = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("hello") });
        assert!(matches!(a, LoopAction::Done { .. }), "no criteria ⇒ no gate, got {a:?}");
        assert!(!sm.observations.iter().any(|o| matches!(o, KernelObservation::CriteriaGateFired { .. })));
    }

    #[test]
    fn criteria_gate_can_be_disabled() {
        let mut sm = sm();
        sm.set_criteria_gate(false);
        let mut task = RuntimeTask::new("g");
        task.criteria = vec!["c1".into()];
        sm.start(task);
        let a = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("done") });
        assert!(matches!(a, LoopAction::Done { .. }));
    }
    // ─── ③ loop-agent pacing trap ────────────────────────────────────────────

    fn loop_sm(spec: crate::types::agent::LoopRoundSpec) -> LoopStateMachine {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};
        let mut sm = sm();
        let mut run_spec = AgentRunSpec::new(
            AgentIdentity::new("loop-agent", "loop-sess"),
            AgentRole::Implement,
            "iterate until done",
        );
        run_spec.loop_round = Some(spec);
        sm.run_spec = Some(run_spec);
        sm.start(RuntimeTask::new("iterate until done"));
        sm
    }

    fn pace_turn(next: &str, delay_ms: Option<u64>) -> Message {
        let mut args = serde_json::json!({ "next": next, "reason": "round done" });
        if let Some(d) = delay_ms {
            args["delay_ms"] = serde_json::json!(d);
        }
        fuse_tool_turn("pace", args)
    }

    #[test]
    fn pace_tool_exposed_only_on_loop_rounds() {
        // No loop_round ⇒ no pace tool.
        let mut plain = sm();
        let action = plain.start(RuntimeTask::new("t"));
        if let LoopAction::CallLLM { tools, .. } = action {
            assert!(!tools.iter().any(|t| t.name.as_str() == "pace"));
        } else {
            panic!("expected CallLLM");
        }
        // loop_round present ⇒ exposed.
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec::default());
        let action = sm.emit_call_llm();
        if let LoopAction::CallLLM { tools, .. } = action {
            assert!(tools.iter().any(|t| t.name.as_str() == "pace"));
        } else {
            panic!("expected CallLLM");
        }
    }

    #[test]
    fn pace_sleep_is_clamped_and_records_coercion() {
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec {
            min_sleep_ms: Some(10_000),
            max_sleep_ms: Some(600_000),
            ..Default::default()
        });
        // Proposes 5ms — clamped up to the 10s floor, coercion recorded.
        let action = sm.feed(LoopEvent::LLMResponse { message: pace_turn("sleep", Some(5)) });
        assert!(matches!(action, LoopAction::CallLLM { ref tools, .. } if tools.is_empty()),
            "an allowed pace strips tools for the final report turn");
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("round report") });
        match done {
            LoopAction::Done { result } => {
                let pace = result.pace_decision.expect("pace attached");
                assert_eq!(pace.action, crate::types::result::PaceAction::Sleep);
                assert_eq!(pace.delay_ms, Some(10_000));
                assert!(pace.coerced_from.as_deref().unwrap_or("").contains("clamped"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn pace_continue_coerced_to_stop_at_max_rounds() {
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec {
            max_rounds: Some(3),
            ..Default::default()
        });
        sm.seed_group_rounds(2); // two rounds already done; this one is the third
        sm.feed(LoopEvent::LLMResponse { message: pace_turn("continue", None) });
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("report") });
        match done {
            LoopAction::Done { result } => {
                let pace = result.pace_decision.expect("pace attached");
                assert_eq!(pace.action, crate::types::result::PaceAction::Stop);
                assert!(pace.coerced_from.as_deref().unwrap_or("").contains("max_rounds"));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn pace_stop_routes_through_criteria_gate_then_honors_the_redecision() {
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec::default());
        sm.ctx.partitions.task_state.criteria =
            vec!["tests green".to_string(), "docs updated".to_string()];
        // First stop proposal → the O4 self-check turn, NOT a round end.
        let action = sm.feed(LoopEvent::LLMResponse { message: pace_turn("stop", None) });
        assert!(matches!(action, LoopAction::CallLLM { ref tools, .. } if !tools.is_empty()),
            "criteria check is a normal working turn, tools stay");
        assert!(sm.ctx.partitions.signals.iter().any(|s| s.contains("[CRITERIA CHECK]")));
        assert!(sm.take_observations().iter().any(|o| matches!(o, KernelObservation::CriteriaGateFired { .. })));
        // The model re-decides stop → honored this time (gate fires once per round).
        sm.feed(LoopEvent::LLMResponse { message: pace_turn("stop", None) });
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("final") });
        match done {
            LoopAction::Done { result } => {
                let pace = result.pace_decision.expect("pace attached");
                assert_eq!(pace.action, crate::types::result::PaceAction::Stop);
                assert!(pace.coerced_from.is_none());
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn round_without_pace_call_falls_back_to_default_action() {
        // Goal loop (default): stop.
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec::default());
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("all done") });
        match done {
            LoopAction::Done { result } => {
                let pace = result.pace_decision.expect("default pace attached");
                assert_eq!(pace.action, crate::types::result::PaceAction::Stop);
            }
            other => panic!("expected Done, got {other:?}"),
        }
        // Cron loop: default_action=sleep uses min_sleep_ms as the interval.
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec {
            default_action: Some("sleep".to_string()),
            min_sleep_ms: Some(300_000),
            ..Default::default()
        });
        let done = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("tick done") });
        match done {
            LoopAction::Done { result } => {
                let pace = result.pace_decision.expect("default pace attached");
                assert_eq!(pace.action, crate::types::result::PaceAction::Sleep);
                assert_eq!(pace.delay_ms, Some(300_000));
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn pace_emits_round_paced_observation_with_seeded_round_number() {
        let mut sm = loop_sm(crate::types::agent::LoopRoundSpec::default());
        sm.seed_group_rounds(4);
        sm.feed(LoopEvent::LLMResponse { message: pace_turn("continue", None) });
        let obs = sm.take_observations();
        let paced = obs.iter().find_map(|o| match o {
            KernelObservation::RoundPaced { round, decision, .. } => Some((*round, decision.clone())),
            _ => None,
        });
        let (round, decision) = paced.expect("RoundPaced emitted");
        assert_eq!(round, 5, "this round = seeded base + 1");
        assert_eq!(decision.action, crate::types::result::PaceAction::Continue);
    }


    // ─── Session entropy: per-turn sample + opt-in watch ───────────────────

    /// Drive one tool turn (name+args → result) and return the boundary observations.
    fn entropy_drive_turn(
        sm: &mut LoopStateMachine,
        args: serde_json::Value,
        is_error: bool,
    ) -> Vec<KernelObservation> {
        let a = sm.feed(LoopEvent::LLMResponse { message: fuse_tool_turn("step", args) });
        assert!(matches!(a, LoopAction::ExecuteTools { .. }), "expected ExecuteTools, got {a:?}");
        let result = ToolResult { is_error, ..fuse_tool_result() };
        sm.feed(LoopEvent::ToolResults { results: vec![result] });
        sm.take_observations()
    }

    fn entropy_sample_of(obs: &[KernelObservation]) -> Option<(f64, f64, f64, u32)> {
        obs.iter().find_map(|o| match o {
            KernelObservation::EntropySample { score, repeat_pressure, failure_rate, rollbacks_in_window, .. } => {
                Some((*score, *repeat_pressure, *failure_rate, *rollbacks_in_window))
            }
            _ => None,
        })
    }

    #[test]
    fn entropy_sample_emitted_every_completed_turn() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("do things"));
        let obs = entropy_drive_turn(&mut sm, serde_json::json!({"n": 1}), false);
        let (score, repeat, failures, rollbacks) =
            entropy_sample_of(&obs).expect("EntropySample at the completed boundary");
        assert!(score < 0.25, "healthy turn must score low, got {score}");
        assert_eq!(repeat, 0.0);
        assert_eq!(failures, 0.0);
        assert_eq!(rollbacks, 0);
    }

    #[test]
    fn entropy_sample_reflects_repeats_and_failures() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("loop badly"));
        // Identical failing call, 4 turns: repeat streak + failure window both climb.
        let mut last = Vec::new();
        for _ in 0..4 {
            last = entropy_drive_turn(&mut sm, serde_json::json!({"same": true}), true);
        }
        let (score, repeat, failures, _) = entropy_sample_of(&last).expect("sample");
        assert!(repeat > 0.0, "identical-call streak must register, got {repeat}");
        assert!((failures - 1.0).abs() < 1e-9, "all results errored, got {failures}");
        assert!(score > 0.3, "disordered run must score high, got {score}");
    }

    #[test]
    fn entropy_watch_alerts_once_and_respects_optin() {
        let mut sm = sm();
        // Watch OFF (default): a fully disordered run emits samples but never alerts.
        sm.start(RuntimeTask::new("loop badly"));
        for _ in 0..4 {
            let obs = entropy_drive_turn(&mut sm, serde_json::json!({"same": true}), true);
            assert!(
                !obs.iter().any(|o| matches!(o, KernelObservation::EntropyAlert { .. })),
                "watch is opt-in — no alert while disabled"
            );
        }

        // Watch ON with a low threshold: exactly one alert (disarm) until re-arm.
        let mut sm = sm2_with_watch(0.2, 0);
        sm.start(RuntimeTask::new("loop badly"));
        let mut alerts = 0;
        for _ in 0..4 {
            let obs = entropy_drive_turn(&mut sm, serde_json::json!({"same": true}), true);
            alerts += obs.iter().filter(|o| matches!(o, KernelObservation::EntropyAlert { .. })).count();
        }
        assert_eq!(alerts, 1, "hysteresis must gate re-fires while the score stays hot");
    }

    fn sm2_with_watch(threshold: f64, cooldown_turns: u32) -> LoopStateMachine {
        let mut sm = sm();
        sm.set_entropy_watch(crate::scheduler::entropy::EntropyWatchConfig {
            enabled: true,
            threshold,
            hysteresis: 0.1,
            cooldown_turns,
            notify_model: false,
        });
        sm
    }

    #[test]
    fn entropy_watch_notify_model_routes_a_heartbeat_signal() {
        let mut sm = sm2_with_watch(0.2, 0);
        let mut cfg = sm.entropy_watch_config();
        cfg.notify_model = true;
        sm.set_entropy_watch(cfg);
        sm.start(RuntimeTask::new("loop badly"));

        let mut saw_alert = false;
        for _ in 0..4 {
            let obs = entropy_drive_turn(&mut sm, serde_json::json!({"same": true}), true);
            if obs.iter().any(|o| matches!(o, KernelObservation::EntropyAlert { .. })) {
                saw_alert = true;
                // The self-signal went through the normal dispatch: disposition observed…
                assert!(obs.iter().any(|o| matches!(o, KernelObservation::SignalDisposed { .. })));
                // …and (High while running ⇒ Interrupt) the directive reached the model channel.
                assert!(
                    sm.ctx.partitions.signals.iter().any(|s| s.contains("[entropy]")),
                    "entropy directive must land in signals: {:?}",
                    sm.ctx.partitions.signals
                );
            }
        }
        assert!(saw_alert, "low threshold + disordered run must alert");
    }

    #[test]
    fn entropy_rollbacks_accrue_into_the_next_completed_turn() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("fatal then recover"));
        // Turn A: fatal tool result ⇒ rollback, no boundary sample.
        let a = sm.feed(LoopEvent::LLMResponse {
            message: fuse_tool_turn("step", serde_json::json!({"n": 1})),
        });
        assert!(matches!(a, LoopAction::ExecuteTools { .. }));
        sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult { is_fatal: true, ..fuse_tool_result() }],
        });
        let rolled = sm.take_observations();
        assert!(rolled.iter().any(|o| matches!(o, KernelObservation::Rollbacked { .. })));
        assert!(entropy_sample_of(&rolled).is_none(), "rolled-back turn has no boundary sample");

        // Turn B completes: the rollback shows up in its window.
        let obs = entropy_drive_turn(&mut sm, serde_json::json!({"n": 2}), false);
        let (_, _, _, rollbacks) = entropy_sample_of(&obs).expect("sample");
        assert_eq!(rollbacks, 1, "turn A's rollback must land in turn B's window");
    }
