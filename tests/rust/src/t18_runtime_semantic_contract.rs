#[test]
fn spc_028_shared_semantic_contract_fixture() {
    let raw = include_str!("../../../tests/fixtures/runtime-language/semantic-contract.json");
    let fixture: serde_json::Value = serde_json::from_str(raw).expect("valid semantic contract");
    assert_eq!(fixture["version"], "0.2.75");
    assert!(
        fixture["public"]
            .as_array()
            .unwrap()
            .iter()
            .any(|term| term == "Agent")
    );
    assert_eq!(fixture["authorities"]["AgentDefinition"], "public-agent");
}
