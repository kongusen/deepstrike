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
