use std::fs;
use std::path::PathBuf;
use serde::de::DeserializeOwned;
use serde::Serialize;
use deepstrike_core::runtime::{KernelInput, KernelStep, KernelObservation};

fn load_fixture(filename: &str) -> String {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = PathBuf::from(manifest_dir)
        .join("../fixtures/abi")
        .join(filename);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture at {}: {}", path.display(), e))
}

fn assert_roundtrip<T>(filename: &str)
where
    T: Serialize + DeserializeOwned + std::fmt::Debug,
{
    let raw = load_fixture(filename);
    
    // Deserialize
    let parsed: T = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("failed to deserialize {} as {}: {}", filename, std::any::type_name::<T>(), e));
    
    // Serialize back to Value to compare structures, avoiding whitespace issues
    let reserialized = serde_json::to_value(&parsed).unwrap();
    let expected: serde_json::Value = serde_json::from_str(&raw).unwrap();
    
    assert_eq!(
        reserialized, expected,
        "roundtrip failed for {}. Reserialized: {:#?}, Expected: {:#?}",
        filename, reserialized, expected
    );
}

#[test]
fn test_input_start_run_fixture() {
    assert_roundtrip::<KernelInput>("input_start_run.json");
}

#[test]
fn test_v1_input_fixture_returns_version_mismatch_fault() {
    use deepstrike_core::runtime::{KernelFaultCode, KernelRuntime};
    use deepstrike_core::scheduler::policy::SchedulerBudget;

    let raw = load_fixture("input_start_run_v1.json");
    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    let step = runtime.step_json(&raw).expect("v1 JSON remains valid JSON");

    assert!(matches!(
        step.faults.as_slice(),
        [fault] if fault.code == KernelFaultCode::VersionMismatch
    ));
}

#[test]
fn test_input_tool_results_fixture() {
    assert_roundtrip::<KernelInput>("input_tool_results.json");
}

#[test]
fn test_step_call_provider_fixture() {
    assert_roundtrip::<KernelStep>("step_call_provider.json");
}

#[test]
fn test_step_execute_tool_fixture() {
    assert_roundtrip::<KernelStep>("step_execute_tool.json");
}

#[test]
fn test_step_done_fixture() {
    assert_roundtrip::<KernelStep>("step_done.json");
}

#[test]
fn test_input_push_artifact_fixture() {
    assert_roundtrip::<KernelInput>("input_push_artifact.json");
}

#[test]
fn test_observation_compressed_fixture() {
    assert_roundtrip::<KernelObservation>("observation_compressed.json");
}

#[test]
fn test_input_spawn_sub_agent_fixture() {
    assert_roundtrip::<KernelInput>("input_spawn_sub_agent.json");
}

#[test]
fn test_observation_agent_process_changed_fixture() {
    assert_roundtrip::<KernelObservation>("observation_agent_process_changed.json");
}

#[test]
fn test_observation_checkpoint_taken_fixture() {
    assert_roundtrip::<KernelObservation>("observation_checkpoint_taken.json");
}

#[test]
fn test_observation_renewed_fixture() {
    assert_roundtrip::<KernelObservation>("observation_renewed.json");
}

#[test]
fn test_observation_rollbacked_fixture() {
    assert_roundtrip::<KernelObservation>("observation_rollbacked.json");
}

#[test]
fn test_observation_capability_changed_fixture() {
    assert_roundtrip::<KernelObservation>("observation_capability_changed.json");
}

#[test]
fn test_observation_milestone_advanced_fixture() {
    assert_roundtrip::<KernelObservation>("observation_milestone_advanced.json");
}

#[test]
fn test_observation_milestone_blocked_fixture() {
    assert_roundtrip::<KernelObservation>("observation_milestone_blocked.json");
}

#[test]
fn spawn_sub_agent_fixture_updates_process_table_via_kernel() {
    use deepstrike_core::runtime::{KernelInput, KernelInputEvent, KernelObservation, KernelRuntime};
    use deepstrike_core::scheduler::policy::SchedulerBudget;
    use deepstrike_core::types::task::RuntimeTask;

    let raw = load_fixture("input_spawn_sub_agent.json");
    let input: KernelInput = serde_json::from_str(&raw).expect("deserialize spawn input");

    let mut runtime = KernelRuntime::new(SchedulerBudget::default());
    runtime.step(KernelInput::new(KernelInputEvent::StartRun {
        task: RuntimeTask::new("parent"),
        run_spec: None,
    }));
    runtime.state_machine_mut().take_observations();

    let KernelInputEvent::SpawnSubAgent { spec, parent_session_id } = input.event else {
        panic!("expected spawn_sub_agent event");
    };
    let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
        spec,
        parent_session_id,
    }));

    assert!(step.actions.is_empty());
    assert!(step.observations.iter().any(|o| matches!(
        o,
        KernelObservation::AgentProcessChanged { agent_id, state, .. }
            if agent_id == "worker" && state == "running"
    )));
}
