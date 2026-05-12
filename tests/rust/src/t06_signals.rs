use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
use deepstrike_core::types::policy::SignalDisposition;

fn normal_signal(summary: &str) -> RuntimeSignal {
    RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Normal, summary)
}

fn critical_signal(summary: &str) -> RuntimeSignal {
    RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::Critical, summary)
}

// ─── Basic routing ──────────────────────────────────────────────────────────

#[test]
fn normal_signal_queued() {
    let mut router = SignalRouter::new(100);
    let d = router.ingest(normal_signal("job"), false);
    assert_eq!(d, SignalDisposition::Queue);
    assert_eq!(router.depth(), 1);
}

#[test]
fn critical_signal_interrupts_now() {
    let mut router = SignalRouter::new(100);
    let d = router.ingest(critical_signal("fire"), true);
    assert_eq!(d, SignalDisposition::InterruptNow);
    assert_eq!(router.depth(), 0);
}

#[test]
fn next_returns_queued_signal() {
    let mut router = SignalRouter::new(100);
    router.ingest(normal_signal("task"), false);
    let sig = router.next().unwrap();
    assert_eq!(sig.summary.as_str(), "task");
    assert_eq!(router.depth(), 0);
}

#[test]
fn next_returns_none_when_empty() {
    let mut router = SignalRouter::new(100);
    assert!(router.next().is_none());
}

// ─── Deduplication ──────────────────────────────────────────────────────────

#[test]
fn deduplicates_by_key() {
    let mut router = SignalRouter::new(100);
    let sig = normal_signal("tick").with_dedupe("cron-tick-1");

    let d1 = router.ingest(sig.clone(), false);
    assert_ne!(d1, SignalDisposition::Ignore);

    let d2 = router.ingest(sig, false);
    assert_eq!(d2, SignalDisposition::Ignore);
}

#[test]
fn different_dedupe_keys_not_deduped() {
    let mut router = SignalRouter::new(100);
    let s1 = normal_signal("a").with_dedupe("key-1");
    let s2 = normal_signal("b").with_dedupe("key-2");

    let d1 = router.ingest(s1, false);
    let d2 = router.ingest(s2, false);
    assert_ne!(d1, SignalDisposition::Ignore);
    assert_ne!(d2, SignalDisposition::Ignore);
    assert_eq!(router.depth(), 2);
}

#[test]
fn no_dedupe_key_never_deduplicates() {
    let mut router = SignalRouter::new(100);
    let d1 = router.ingest(normal_signal("a"), false);
    let d2 = router.ingest(normal_signal("a"), false);
    assert_ne!(d1, SignalDisposition::Ignore);
    assert_ne!(d2, SignalDisposition::Ignore);
}

#[test]
fn clear_dedup_allows_reingest() {
    let mut router = SignalRouter::new(100);
    let sig = normal_signal("tick").with_dedupe("key-1");

    router.ingest(sig.clone(), false);
    assert_eq!(router.ingest(sig.clone(), false), SignalDisposition::Ignore);

    router.clear_dedup();
    assert_ne!(router.ingest(sig, false), SignalDisposition::Ignore);
}

// ─── Queue capacity ─────────────────────────────────────────────────────────

#[test]
fn full_queue_drops_signal() {
    let mut router = SignalRouter::new(1);
    assert_eq!(router.ingest(normal_signal("first"), false), SignalDisposition::Queue);
    assert_eq!(router.ingest(normal_signal("second"), false), SignalDisposition::Dropped);
    assert_eq!(router.depth(), 1);
}

#[test]
fn drain_queue_makes_room() {
    let mut router = SignalRouter::new(1);
    router.ingest(normal_signal("first"), false);
    router.next(); // drain
    assert_eq!(router.ingest(normal_signal("second"), false), SignalDisposition::Queue);
}

// ─── Urgency-based policy ───────────────────────────────────────────────────

#[test]
fn high_urgency_when_running_interrupts() {
    let mut router = SignalRouter::new(100);
    let sig = RuntimeSignal::new(
        SignalSource::Gateway, SignalType::Alert, Urgency::High, "warn"
    );
    let d = router.ingest(sig, true);
    assert!(d == SignalDisposition::Interrupt || d == SignalDisposition::InterruptNow);
}

#[test]
fn low_urgency_observed_or_queued() {
    let mut router = SignalRouter::new(100);
    let sig = RuntimeSignal::new(
        SignalSource::Cron, SignalType::Event, Urgency::Low, "bg"
    );
    let d = router.ingest(sig, false);
    assert!(d == SignalDisposition::Queue || d == SignalDisposition::Observe);
}

// ─── RuntimeSignal builders ─────────────────────────────────────────────────

#[test]
fn signal_builder_chain() {
    let sig = RuntimeSignal::new(SignalSource::Custom, SignalType::Job, Urgency::Normal, "deploy")
        .with_payload(serde_json::json!({"env": "prod"}))
        .with_dedupe("deploy-1")
        .with_timestamp(1234567890);

    assert_eq!(sig.summary.as_str(), "deploy");
    assert_eq!(sig.payload["env"], "prod");
    assert_eq!(sig.dedupe_key.as_deref(), Some("deploy-1"));
    assert_eq!(sig.timestamp_ms, 1234567890);
}

#[test]
fn signal_source_variants() {
    let _cron = SignalSource::Cron;
    let _gw = SignalSource::Gateway;
    let _hb = SignalSource::Heartbeat;
    let _custom = SignalSource::Custom;
}

#[test]
fn signal_type_variants() {
    let _event = SignalType::Event;
    let _job = SignalType::Job;
    let _alert = SignalType::Alert;
}

#[test]
fn urgency_ordering() {
    assert!(Urgency::Low < Urgency::Normal);
    assert!(Urgency::Normal < Urgency::High);
    assert!(Urgency::High < Urgency::Critical);
}

// ─── SDK-level signals ──────────────────────────────────────────────────────

#[test]
fn scheduled_prompt_to_signal() {
    let prompt = deepstrike_sdk::ScheduledPrompt::new("daily standup", 1_700_000_000_000);
    let sig = prompt.to_signal();
    assert_eq!(sig.kind, "scheduled");
    assert_eq!(sig.payload["goal"], "daily standup");
    assert_eq!(sig.payload["run_at_ms"], 1_700_000_000_000u64);
}

#[test]
fn signal_gateway_subscribe_ingest() {
    let gw = deepstrike_sdk::SignalGateway::new();
    let _rx = gw.subscribe();
    gw.ingest(deepstrike_sdk::RuntimeSignal {
        kind: "webhook".into(),
        payload: serde_json::json!({"event": "push"}),
        priority: 1,
    });
    gw.destroy();
}
