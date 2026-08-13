//! spc_003 debt closure: empirical evidence for the acceptance criterion "Effect/Channel/
//! Approval/Timer wakeup paths hit `WaitIndex` directly, no full-table scan (benchmark verifies
//! the complexity)". No card in spc_003-01..06 required this; it closes that documented gap.
//!
//! Method: insert N unrelated (task, key) pairs, then time M targeted lookups/wakes of ONE
//! specific key at two very different scales of N. A `HashMap`/`BTreeMap`-backed O(1)-average
//! index's targeted-lookup cost is independent of N; a full-table scan's is not — so comparing
//! total time for the same M operations across a 100x difference in N is a direct empirical
//! check, not a proxy metric.

use std::hint::black_box;
use std::time::Instant;

use deepstrike_core::runtime::kernel::wire::EffectId;
use deepstrike_core::scheduler::tcb::{TaskId, WaitCondition};
use deepstrike_core::scheduler::wait_index::{WaitIndex, WaitKey};

const LOOKUPS: usize = 50_000;

/// Build an index with `n` unrelated single-task effect waits, plus one extra task registered on
/// `target_key` — the key every timed lookup/wake in this benchmark targets.
fn seeded_index(n: usize, target_key: &WaitKey) -> WaitIndex {
    let mut index = WaitIndex::new();
    for i in 0..n {
        let effect = EffectId::new(format!("unrelated-{i}")).expect("valid effect id");
        index.insert(
            TaskId::from(format!("task-{i}")),
            &WaitCondition::Effect(effect),
        );
    }
    let WaitKey::Effect(target_effect) = target_key else {
        unreachable!("this benchmark only exercises the Effect key")
    };
    index.insert(
        TaskId::from("target-task"),
        &WaitCondition::Effect(target_effect.clone()),
    );
    index
}

fn time_lookups(index: &WaitIndex, key: &WaitKey, iterations: usize) -> std::time::Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        black_box(index.lookup(key));
    }
    started.elapsed()
}

/// `wake` mutates (removes), so each iteration re-registers a single task under `key` (an O(1)
/// `insert`) immediately before waking it — the N unrelated background entries are seeded once,
/// outside the timed region, so the timer only ever measures insert+wake against a table already
/// at size N, never the O(n) cost of building that table.
fn time_wakes(n_unrelated: usize, key: &WaitKey, iterations: usize) -> std::time::Duration {
    let WaitKey::Effect(effect) = key else {
        unreachable!("this benchmark only exercises the Effect key")
    };
    let mut index = WaitIndex::new();
    for i in 0..n_unrelated {
        let unrelated = EffectId::new(format!("unrelated-{i}")).expect("valid effect id");
        index.insert(
            TaskId::from(format!("task-{i}")),
            &WaitCondition::Effect(unrelated),
        );
    }

    let started = Instant::now();
    for _ in 0..iterations {
        index.insert(
            TaskId::from("target-task"),
            &WaitCondition::Effect(effect.clone()),
        );
        black_box(index.wake(&WaitKey::Effect(effect.clone())));
    }
    started.elapsed()
}

fn report(
    label: &str,
    small: std::time::Duration,
    large: std::time::Duration,
    small_n: usize,
    large_n: usize,
) {
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-9);
    println!(
        "{label}: N={small_n} -> {:.3} ms; N={large_n} ({}x larger) -> {:.3} ms; ratio={:.2}x",
        small.as_secs_f64() * 1_000.0,
        large_n / small_n.max(1),
        large.as_secs_f64() * 1_000.0,
        ratio,
    );
}

fn main() {
    let target_effect = EffectId::new("target").expect("valid effect id");
    let target_key = WaitKey::Effect(target_effect);

    // ---- lookup: O(1) average, no full-table scan --------------------------------------------
    let small_index = seeded_index(1_000, &target_key);
    let large_index = seeded_index(100_000, &target_key);

    // Warm up (first access can pay allocator/cache costs unrelated to the algorithm).
    time_lookups(&small_index, &target_key, 1_000);
    time_lookups(&large_index, &target_key, 1_000);

    let small_lookup = time_lookups(&small_index, &target_key, LOOKUPS);
    let large_lookup = time_lookups(&large_index, &target_key, LOOKUPS);
    report("lookup", small_lookup, large_lookup, 1_000, 100_000);

    // A full O(n) scan across a 100x larger table would show ~100x total time for the same
    // number of targeted lookups. An O(1)-average hash lookup should not — budget a generous 5x
    // to absorb hashing/cache noise while still catching an accidental linear scan.
    assert!(
        large_lookup.as_secs_f64() <= small_lookup.as_secs_f64() * 5.0,
        "lookup time scaled with table size (small={small_lookup:?}, large={large_lookup:?}) — \
         looks like a full-table scan, not an indexed lookup"
    );

    // ---- wake: same O(1)-average claim, on the mutating path -------------------------------
    const WAKE_ITERATIONS: usize = 2_000;
    let small_wake = time_wakes(1_000, &target_key, WAKE_ITERATIONS);
    let large_wake = time_wakes(100_000, &target_key, WAKE_ITERATIONS);
    report("wake", small_wake, large_wake, 1_000, 100_000);

    assert!(
        large_wake.as_secs_f64() <= small_wake.as_secs_f64() * 5.0,
        "wake time scaled with table size (small={small_wake:?}, large={large_wake:?}) — \
         looks like a full-table scan, not an indexed wake"
    );
}
