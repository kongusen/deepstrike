//! Performance baseline for the host-side verifiable report path.

use std::hint::black_box;
use std::time::Instant;

use deepstrike_core::runtime::verifiable::{EvidenceBundle, VerifiableOperation};

fn fixture_records() -> Vec<Vec<u8>> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/kernel-wire/golden_record_chain.json"
    ))
    .expect("record fixture is valid JSON");
    fixture["links"]
        .as_array()
        .expect("record fixture has links")
        .iter()
        .map(|link| serde_json::to_vec(&link["record"]).expect("record serializes"))
        .collect()
}

fn main() {
    let records = fixture_records();
    let iterations = 100usize;
    let started = Instant::now();
    for _ in 0..iterations {
        let operation = VerifiableOperation::new(
            "op-record-1",
            EvidenceBundle::new(records.clone(), Vec::new(), Vec::new()),
        );
        black_box(operation.inspect(false));
    }
    let elapsed = started.elapsed();
    let micros = elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64;
    println!(
        "verifiable inspect: {iterations} operations in {:.3} ms ({micros:.3} us/op), {} journal records",
        elapsed.as_secs_f64() * 1_000.0,
        records.len(),
    );
    assert!(elapsed > std::time::Duration::ZERO);
}
