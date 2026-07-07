//! Session-entropy sampling — the kernel-side measurement behind a host "heartbeat
//! entropy watch" source.
//!
//! "Entropy" here is session *disorder*: the degree to which a run is churning without
//! converging — repeating itself, failing tool calls, rolling turns back, and running out
//! of context headroom. The kernel already detects each symptom in isolation (2c STOP /
//! RepeatFuse / rollback / eviction); this module folds them into one per-turn sample the
//! host can subscribe to, so an external supervisor (heartbeat) can decide *its* policy —
//! e.g. inject a corrective note — without re-deriving kernel state from the audit log.
//!
//! Two honesty rules govern the shape:
//! - The component vector is the contract; `score` is only a *versioned* default fold
//!   ([`ENTROPY_SCORE_VERSION`]). Hosts that care should threshold on components.
//! - Measurement is unconditional (one sample per completed turn boundary, like
//!   `CheckpointTaken`); only the alert — a kernel-side threshold decision — is opt-in
//!   via [`EntropyWatchConfig`].

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Version of the default `score` fold. Bump when the formula or weights change so hosts
/// thresholding on `score` can detect the semantics shift.
pub const ENTROPY_SCORE_VERSION: u32 = 1;

/// Sliding window (in completed turns) for the failure/rollback components.
pub const ENTROPY_WINDOW_TURNS: usize = 8;

/// Saturation point for the rollback component: this many rollbacks inside the window
/// reads as fully disordered (1.0) on that axis.
const ROLLBACK_SATURATION: f64 = 3.0;

/// Opt-in threshold watch over the per-turn entropy score (③). When the score crosses
/// `threshold` the kernel emits an `EntropyAlert` observation — at most once per crossing
/// (hysteresis re-arm) and never more often than `cooldown_turns`. With `notify_model`
/// the alert is *also* routed through the kernel's own signal dispatch as a
/// `Heartbeat/Alert` [`RuntimeSignal`](crate::types::signal::RuntimeSignal), so the model
/// sees a durable `[SIGNAL]` directive at the next boundary. Default OFF: the primary
/// consumer is the host supervisor, which can inject a task-aware note itself — an
/// unconditional self-nudge risks a feedback loop (the note churns context → more entropy).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EntropyWatchConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Alert when `score >= threshold`.
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// Re-arm only after the score falls below `threshold - hysteresis` (anti-flap).
    #[serde(default = "default_hysteresis")]
    pub hysteresis: f64,
    /// Minimum completed turns between two alerts.
    #[serde(default = "default_cooldown_turns")]
    pub cooldown_turns: u32,
    /// Also self-signal the model (Heartbeat/Alert, High urgency) when the alert fires.
    #[serde(default)]
    pub notify_model: bool,
}

fn default_threshold() -> f64 { 0.65 }
fn default_hysteresis() -> f64 { 0.1 }
fn default_cooldown_turns() -> u32 { 4 }

impl Default for EntropyWatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_threshold(),
            hysteresis: default_hysteresis(),
            cooldown_turns: default_cooldown_turns(),
            notify_model: false,
        }
    }
}

/// One per-turn entropy measurement. All normalized components are in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntropySample {
    pub turn: u32,
    /// Versioned default fold of the components (see [`ENTROPY_SCORE_VERSION`]).
    pub score: f64,
    /// Context pressure after this boundary's eviction pass (`ContextManager::rho`).
    pub rho: f64,
    /// Consecutive-identical-turn streak, normalized against the RepeatFuse deny rung
    /// (0 when the streak is 1 — a first occurrence is not repetition — or the fuse is off).
    pub repeat_pressure: f64,
    /// Errored tool results / total tool results over the window.
    pub failure_rate: f64,
    /// Raw rollback count inside the window (normalize with `window_turns`).
    pub rollbacks_in_window: u32,
    /// Effective window size (completed turns currently held, ≤ [`ENTROPY_WINDOW_TURNS`]).
    pub window_turns: u32,
}

/// Sliding-window state feeding [`EntropySample`]. Owned by the state machine; fed at the
/// completed-turn boundary (`ToolResults`) and by `rollback()`. Deliberately NOT part of
/// the turn checkpoint: a rollback must not launder the disorder it just evidenced —
/// the same reasoning as the RepeatFuse streak.
#[derive(Debug, Default)]
pub struct EntropyTracker {
    /// Per completed turn: (errored results, total results).
    turn_stats: VecDeque<(u32, u32)>,
    /// Per completed turn: rollbacks observed since the previous completed boundary.
    rollback_stats: VecDeque<u32>,
    /// Rollbacks seen since the last completed boundary (turns that roll back return
    /// early and never reach the sample point — they accrue here until one completes).
    rollbacks_pending: u32,
    /// ③ watch state: armed ⇒ the next threshold crossing may alert.
    disarmed: bool,
    last_alert_turn: Option<u32>,
}

impl EntropyTracker {
    /// Record a rollback (any reason). Called from the state machine's `rollback()`.
    pub fn note_rollback(&mut self) {
        self.rollbacks_pending += 1;
    }

    /// Fold this boundary's outcomes into the window and produce the turn's sample.
    /// `repeat_streak` is the RepeatFuse consecutive-signature count (0/1 ⇒ no repetition).
    pub fn sample(
        &mut self,
        turn: u32,
        rho: f64,
        repeat_streak: u32,
        repeat_deny_after: u32,
        errored_results: u32,
        total_results: u32,
    ) -> EntropySample {
        self.turn_stats.push_back((errored_results, total_results));
        self.rollback_stats.push_back(std::mem::take(&mut self.rollbacks_pending));
        while self.turn_stats.len() > ENTROPY_WINDOW_TURNS {
            self.turn_stats.pop_front();
        }
        while self.rollback_stats.len() > ENTROPY_WINDOW_TURNS {
            self.rollback_stats.pop_front();
        }

        let (errors, totals) = self
            .turn_stats
            .iter()
            .fold((0u32, 0u32), |(e, t), (te, tt)| (e + te, t + tt));
        let rollbacks_in_window: u32 = self.rollback_stats.iter().sum();

        let rho = rho.clamp(0.0, 1.0);
        // Streak 1 = first occurrence = zero repetition; pressure saturates at the deny rung.
        let repeat_pressure = (f64::from(repeat_streak.saturating_sub(1))
            / f64::from(repeat_deny_after.max(1)))
        .clamp(0.0, 1.0);
        let failure_rate = if totals == 0 { 0.0 } else { f64::from(errors) / f64::from(totals) };
        let rollback_term = (f64::from(rollbacks_in_window) / ROLLBACK_SATURATION).clamp(0.0, 1.0);

        // v1 fold: repetition and failures dominate (they are the direct "no forward
        // progress" evidence), pressure and rollbacks corroborate.
        let score = 0.35 * repeat_pressure + 0.30 * failure_rate + 0.20 * rho + 0.15 * rollback_term;

        EntropySample {
            turn,
            score,
            rho,
            repeat_pressure,
            failure_rate,
            rollbacks_in_window,
            window_turns: self.turn_stats.len() as u32,
        }
    }

    /// ③ threshold decision for this sample: hysteresis re-arm + cooldown. Mutates the
    /// watch state; returns `true` when an alert should fire.
    pub fn should_alert(&mut self, config: &EntropyWatchConfig, sample: &EntropySample) -> bool {
        if !config.enabled {
            return false;
        }
        if sample.score < config.threshold {
            if sample.score < config.threshold - config.hysteresis {
                self.disarmed = false;
            }
            return false;
        }
        if self.disarmed {
            return false;
        }
        let cooled_down = self
            .last_alert_turn
            .is_none_or(|t| sample.turn.saturating_sub(t) >= config.cooldown_turns);
        if !cooled_down {
            return false;
        }
        self.disarmed = true;
        self.last_alert_turn = Some(sample.turn);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet_sample(tracker: &mut EntropyTracker, turn: u32) -> EntropySample {
        tracker.sample(turn, 0.1, 1, 5, 0, 2)
    }

    #[test]
    fn healthy_turn_scores_near_zero() {
        let mut t = EntropyTracker::default();
        let s = quiet_sample(&mut t, 1);
        assert!(s.score < 0.05, "healthy turn score {} should be ~0", s.score);
        assert_eq!(s.repeat_pressure, 0.0);
        assert_eq!(s.failure_rate, 0.0);
        assert_eq!(s.rollbacks_in_window, 0);
    }

    #[test]
    fn repetition_and_failures_raise_the_score() {
        let mut t = EntropyTracker::default();
        // 4-streak against deny_after=5, every result errored, high pressure.
        let s = t.sample(3, 0.9, 4, 5, 2, 2);
        assert!(s.score > 0.6, "disordered turn score {} should be high", s.score);
        assert!((s.repeat_pressure - 0.6).abs() < 1e-9);
        assert!((s.failure_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn failure_rate_windows_out_old_turns() {
        let mut t = EntropyTracker::default();
        t.sample(1, 0.1, 1, 5, 3, 3); // all-error turn
        for turn in 2..=(ENTROPY_WINDOW_TURNS as u32 + 1) {
            let s = quiet_sample(&mut t, turn);
            if turn <= ENTROPY_WINDOW_TURNS as u32 {
                assert!(s.failure_rate > 0.0, "turn {turn} still inside the window");
            } else {
                assert_eq!(s.failure_rate, 0.0, "turn {turn} should have evicted the errors");
            }
        }
    }

    #[test]
    fn rollbacks_accrue_until_a_boundary_completes() {
        let mut t = EntropyTracker::default();
        t.note_rollback();
        t.note_rollback();
        let s = quiet_sample(&mut t, 2);
        assert_eq!(s.rollbacks_in_window, 2);
        // Consumed into the window — not double-counted next turn (still windowed though).
        let s = quiet_sample(&mut t, 3);
        assert_eq!(s.rollbacks_in_window, 2);
    }

    #[test]
    fn watch_fires_once_then_rearms_below_hysteresis() {
        let cfg = EntropyWatchConfig { enabled: true, threshold: 0.5, hysteresis: 0.1, cooldown_turns: 0, notify_model: false };
        let mut t = EntropyTracker::default();
        let hot = EntropySample { turn: 1, score: 0.7, rho: 0.0, repeat_pressure: 0.0, failure_rate: 0.0, rollbacks_in_window: 0, window_turns: 1 };
        assert!(t.should_alert(&cfg, &hot));
        // Still hot: no re-fire until re-armed.
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 2, ..hot }));
        // Inside the hysteresis band (0.45 ≥ threshold − hysteresis): stays disarmed.
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 3, score: 0.45, ..hot }));
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 4, ..hot }));
        // Below the band: re-arms; the next crossing fires again.
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 5, score: 0.3, ..hot }));
        assert!(t.should_alert(&cfg, &EntropySample { turn: 6, ..hot }));
    }

    #[test]
    fn watch_cooldown_gates_refire_even_after_rearm() {
        let cfg = EntropyWatchConfig { enabled: true, threshold: 0.5, hysteresis: 0.1, cooldown_turns: 5, notify_model: false };
        let mut t = EntropyTracker::default();
        let hot = EntropySample { turn: 1, score: 0.9, rho: 0.0, repeat_pressure: 0.0, failure_rate: 0.0, rollbacks_in_window: 0, window_turns: 1 };
        assert!(t.should_alert(&cfg, &hot));
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 2, score: 0.2, ..hot })); // re-arm
        assert!(!t.should_alert(&cfg, &EntropySample { turn: 3, ..hot })); // armed but cooling
        assert!(t.should_alert(&cfg, &EntropySample { turn: 6, ..hot })); // 6−1 ≥ 5
    }

    #[test]
    fn watch_disabled_never_alerts() {
        let cfg = EntropyWatchConfig::default();
        assert!(!cfg.enabled);
        let mut t = EntropyTracker::default();
        let hot = EntropySample { turn: 1, score: 1.0, rho: 1.0, repeat_pressure: 1.0, failure_rate: 1.0, rollbacks_in_window: 9, window_turns: 8 };
        assert!(!t.should_alert(&cfg, &hot));
    }
}
