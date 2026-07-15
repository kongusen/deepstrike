use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use deepstrike_core::orchestration::workflow::{WorkflowNode, WorkflowSpec};
use deepstrike_core::runtime::kernel::{KernelReliabilityConfig, RunConfig};
use deepstrike_core::runtime::{KernelInput, KernelInputEvent, KernelRuntime};
use deepstrike_core::scheduler::policy::SchedulerBudget;
use deepstrike_core::types::agent::AgentRole;
use deepstrike_core::types::message::Message;
use deepstrike_core::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
use deepstrike_core::types::task::RuntimeTask;

struct CountingAllocator;

static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

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

fn elapsed_ms(label: &str, started: Instant, operations: usize) {
    let elapsed = started.elapsed();
    let allocations = ALLOCATION_COUNT.load(Ordering::Relaxed);
    let allocated_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    println!(
        "{label}: {:.3} ms total, {:.3} us/op ({operations} ops), {allocations} allocations / {allocated_bytes} bytes",
        elapsed.as_secs_f64() * 1_000.0,
        elapsed.as_secs_f64() * 1_000_000.0 / operations.max(1) as f64,
    );
}

fn input(operation: &str, index: usize, event: KernelInputEvent) -> KernelInput {
    KernelInput::correlated(
        operation,
        format!("{operation}-event-{index}"),
        index as u64,
        event,
    )
}

fn main() {
    let mut step_runtime = KernelRuntime::new(SchedulerBudget::default());
    let started = begin_measurement();
    for index in 0..10_000 {
        black_box(step_runtime.step(input(
            "step-baseline",
            index,
            KernelInputEvent::SetMemoryEnabled {
                enabled: index % 2 == 0,
            },
        )));
    }
    elapsed_ms("kernel step", started, 10_000);

    let mut render_runtime = KernelRuntime::new(SchedulerBudget::default());
    for index in 0..1_000 {
        render_runtime.step(input(
            "render-baseline",
            index,
            KernelInputEvent::AddHistoryMessage {
                message: Message::user(format!("history-{index} {}", "x".repeat(256))),
                tokens: Some(64),
            },
        ));
    }
    let started = begin_measurement();
    for _ in 0..100 {
        black_box(render_runtime.render());
    }
    elapsed_ms("large-context render", started, 100);

    let started = begin_measurement();
    black_box(render_runtime.step(input(
        "render-baseline",
        1_001,
        KernelInputEvent::ForceCompact,
    )));
    elapsed_ms("compression", started, 1);

    let nodes = (0..100)
        .map(|index| {
            WorkflowNode::new(
                RuntimeTask::new(format!("node-{index}")),
                AgentRole::Implement,
            )
        })
        .collect();
    let mut workflow_runtime = KernelRuntime::new(SchedulerBudget::default());
    workflow_runtime.step(input(
        "workflow-baseline",
        0,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent"),
            run_spec: None,
        },
    ));
    let started = begin_measurement();
    black_box(workflow_runtime.step(input(
        "workflow-baseline",
        1,
        KernelInputEvent::SubmitWorkflow {
            spec: WorkflowSpec::new(nodes),
            parent_session_id: "parent".into(),
            submitter_agent_id: None,
        },
    )));
    elapsed_ms("large workflow", started, 1);

    let mut signal_runtime = KernelRuntime::new(SchedulerBudget::default());
    signal_runtime.step(input(
        "signal-baseline",
        0,
        KernelInputEvent::ConfigureRun {
            config: RunConfig {
                reliability: Some(KernelReliabilityConfig {
                    snapshot_input_limit: Some(20_000),
                    ..KernelReliabilityConfig::default()
                }),
                ..RunConfig::default()
            },
        },
    ));
    signal_runtime.step(input(
        "signal-baseline",
        1,
        KernelInputEvent::StartRun {
            task: RuntimeTask::new("signals"),
            run_spec: None,
        },
    ));
    let started = begin_measurement();
    for index in 2..10_002 {
        let mut signal = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Event,
            Urgency::Normal,
            format!("signal-{index}"),
        );
        signal.id = uuid::Uuid::from_u128(index as u128);
        black_box(signal_runtime.step(input(
            "signal-baseline",
            index,
            KernelInputEvent::DeliverSignal {
                delivery_id: format!("delivery-{index}"),
                attempt: 1,
                signal,
            },
        )));
    }
    elapsed_ms("signal storm", started, 10_000);

    let started = begin_measurement();
    let encoded = signal_runtime
        .snapshot_json()
        .expect("encode 10k-event snapshot");
    elapsed_ms("snapshot encode", started, 10_002);
    println!("snapshot size: {} bytes", encoded.len());

    let started = begin_measurement();
    black_box(KernelRuntime::restore_snapshot_json(&encoded).expect("decode/replay snapshot"));
    elapsed_ms("10k-event replay + snapshot decode", started, 10_002);
}
