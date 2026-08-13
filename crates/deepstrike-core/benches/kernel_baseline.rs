use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use deepstrike_core::runtime::kernel::wire::{CanonicalKernel, KernelPreparation};
use serde_json::{Value, json};

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct AllocationMeasurement {
    count: u64,
    bytes: u64,
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let replacement = unsafe { System.realloc(pointer, layout, new_size) };
        if !replacement.is_null() {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        replacement
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

fn begin_measurement() -> Instant {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    Instant::now()
}

fn elapsed_ms(label: &str, started: Instant, operations: usize) -> AllocationMeasurement {
    let elapsed = started.elapsed();
    let measurement = AllocationMeasurement {
        count: ALLOCATION_COUNT.load(Ordering::Relaxed),
        bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
    };
    println!(
        "{label}: {:.3} ms total, {:.3} us/op ({operations} ops), {} allocations / {} bytes",
        elapsed.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64() * 1_000_000.0 / operations.max(1) as f64,
        measurement.count,
        measurement.bytes,
    );
    measurement
}

fn assert_allocation_budget(
    label: &str,
    measurement: AllocationMeasurement,
    max_count: u64,
    max_bytes: u64,
) {
    assert!(
        measurement.count <= max_count,
        "{label} allocated {} times; budget is {max_count}",
        measurement.count
    );
    assert!(
        measurement.bytes <= max_bytes,
        "{label} allocated {} bytes; budget is {max_bytes}",
        measurement.bytes
    );
}

fn envelope(
    operation_id: &str,
    input_id: impl Into<String>,
    observed_at_ms: u64,
    input: Value,
) -> Value {
    json!({
        "operation_id": operation_id,
        "input_id": input_id.into(),
        "observed_at_ms": observed_at_ms.to_string(),
        "input": input,
    })
}

fn commit_json(kernel: &mut CanonicalKernel, input: Value) {
    let encoded = serde_json::to_string(&input).expect("canonical input encodes");
    let KernelPreparation::Prepared(prepared) = kernel.prepare_json(&encoded) else {
        panic!("canonical input must prepare: {encoded}");
    };
    let appended_head = prepared.record.record_digest().clone();
    let committed = kernel
        .commit(&prepared.token, &appended_head)
        .expect("canonical input commits");
    black_box(committed);
}

fn configure(kernel: &mut CanonicalKernel, operation_id: &str) {
    let config = json!({
        "host_effect_support": {
            "supported": ["call_provider", "spawn_tasks", "preempt_tasks"]
        },
        "signal_policy": { "queue_max": 64 }
    });
    commit_json(
        kernel,
        envelope(
            operation_id,
            "configure",
            1_700_000_000_000,
            json!({ "kind": "configure_operation", "config": config }),
        ),
    );
}

fn start_agent(kernel: &mut CanonicalKernel, operation_id: &str, input_id: &str, messages: Value) {
    commit_json(
        kernel,
        envelope(
            operation_id,
            input_id,
            1_700_000_000_001,
            json!({
                "kind": "start_operation",
                "entry": {
                    "kind": "agent",
                    "task": { "goal": "benchmark canonical construction" }
                },
                "initial_context": { "messages": messages }
            }),
        ),
    );
}

fn main() {
    let f1 = deepstrike_core::benchmark::f1_critical_path_skew();
    assert!(f1.policy_makespan < f1.id_order_makespan);
    let f2 = deepstrike_core::benchmark::f2_loop_fairness();
    assert_eq!(f2.waiting_rounds, 0);
    let f3 = deepstrike_core::benchmark::f3_termination_dependency_matrix();
    assert_eq!(f3.cases_checked, 12);
    println!("DAG gates: F1={f1:?}, F2={f2:?}, F3={f3:?}");

    let started = begin_measurement();
    for index in 0..1_000 {
        let operation_id = format!("operation-{index}");
        let mut kernel = CanonicalKernel::default();
        configure(&mut kernel, &operation_id);
        start_agent(&mut kernel, &operation_id, "start", json!([]));
    }
    let measured = elapsed_ms("canonical operation construction", started, 1_000);
    assert_allocation_budget(
        "canonical operation construction",
        measured,
        1_500_000,
        310_000_000,
    );

    let mut context_kernel = CanonicalKernel::default();
    configure(&mut context_kernel, "large-context");
    let messages = Value::Array(
        (0..1_000)
            .map(|index| {
                json!({
                    "role": "user",
                    "content": format!("history-{index} {}", "x".repeat(256)),
                    "tokens": 64
                })
            })
            .collect(),
    );
    let started = begin_measurement();
    start_agent(&mut context_kernel, "large-context", "start", messages);
    let measured = elapsed_ms("large-context canonical start", started, 1);
    assert_allocation_budget(
        "large-context canonical start",
        measured,
        100_000,
        34_000_000,
    );

    let started = begin_measurement();
    commit_json(
        &mut context_kernel,
        envelope(
            "large-context",
            "force-compact",
            1_700_000_000_002,
            json!({
                "kind": "host_control",
                "command": { "kind": "force_compact" }
            }),
        ),
    );
    let measured = elapsed_ms("canonical forced compression", started, 1);
    assert_allocation_budget("canonical forced compression", measured, 32_000, 6_000_000);

    let mut workflow_kernel = CanonicalKernel::default();
    configure(&mut workflow_kernel, "large-workflow");
    let nodes = (0..100)
        .map(|index| {
            json!({
                "node_id": format!("node-{index}"),
                "task": { "goal": format!("execute node {index}") }
            })
        })
        .collect::<Vec<_>>();
    let started = begin_measurement();
    commit_json(
        &mut workflow_kernel,
        envelope(
            "large-workflow",
            "start",
            1_700_000_000_001,
            json!({
                "kind": "start_operation",
                "entry": {
                    "kind": "workflow",
                    "spec": { "name": "benchmark", "nodes": nodes }
                },
                "initial_context": {}
            }),
        ),
    );
    let measured = elapsed_ms("100-node canonical workflow start", started, 1);
    assert_allocation_budget(
        "100-node canonical workflow start",
        measured,
        16_000,
        2_800_000,
    );

    let mut signal_kernel = CanonicalKernel::default();
    configure(&mut signal_kernel, "signal-storm");
    start_agent(&mut signal_kernel, "signal-storm", "start", json!([]));
    let started = begin_measurement();
    for index in 0..1_000 {
        commit_json(
            &mut signal_kernel,
            envelope(
                "signal-storm",
                format!("signal-input-{index}"),
                1_700_000_001_000 + index,
                json!({
                    "kind": "deliver_external_event",
                    "event": {
                        "kind": "deliver_signal",
                        "delivery_id": format!("delivery-{index}"),
                        "attempt": 1,
                        "signal": {
                            "signal_id": format!("signal-{index}"),
                            "source": "gateway",
                            "target": { "kind": "operation" },
                            "urgency": "normal",
                            "payload": { "index": index }
                        }
                    }
                }),
            ),
        );
    }
    let measured = elapsed_ms("canonical signal storm", started, 1_000);
    assert_allocation_budget("canonical signal storm", measured, 2_400_000, 310_000_000);
}
