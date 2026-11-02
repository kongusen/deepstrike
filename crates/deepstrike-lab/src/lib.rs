//! Plan task 4.3 · offline comparison of context-policy variants.
//!
//! A benchmark driver (`benchmark/context-lifecycle/*.mjs`) runs each task under one variant (a
//! checkout, a `ContextPolicy`, an edge mode) and writes one [`Trial`] per line: whether the task
//! succeeded, what it cost, the kernel's token estimates next to the provider's counts, and the
//! run's `context_lifecycle_reported` events. This crate reads those lines, summarizes each
//! variant ([`Summary`]) and checks a candidate against a baseline ([`Gate`]).
//!
//! The trial format is deliberately loose: every field but `scenario`, `variant` and `task` has a
//! default, and unknown fields are ignored, so a trial recorded on an older checkout (which
//! reports fewer lifecycle fields) still compares against a newer one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One task run under one variant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Trial {
    /// The benchmark the task belongs to; variants are compared within a scenario.
    pub scenario: String,
    pub variant: String,
    pub task: String,
    #[serde(default)]
    pub success: bool,
    /// Why the task could not be graded (a provider error, a run that did not complete).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub cost: Cost,
    /// The kernel's estimate of each provider request next to the provider's count of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_estimates: Vec<TokenEstimate>,
    /// Every `context_lifecycle_reported` event of the task's runs (a workflow has one per node).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle: Vec<LifecycleReport>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Characters of rendered context sent across all provider calls.
    #[serde(default)]
    pub sent_chars: u64,
    #[serde(default)]
    pub calls: u64,
    /// Provider-reported usage, summed; absent when the provider reports none.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenEstimate {
    pub estimated: u64,
    pub observed: u64,
}

/// The parts of a `context_lifecycle_reported` event the lab reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LifecycleReport {
    #[serde(default)]
    pub knowledge_entries: u64,
    #[serde(default)]
    pub knowledge_referenced: u64,
    #[serde(default)]
    pub regret: u64,
    #[serde(default)]
    pub waste_tokens: u64,
    #[serde(default)]
    pub cache_loss_tokens: u64,
    #[serde(default)]
    pub recalled: u64,
    #[serde(default)]
    pub recall_expanded: u64,
    #[serde(default)]
    pub history: Vec<OriginTally>,
    #[serde(default)]
    pub history_items: Vec<HistoryItem>,
    #[serde(default)]
    pub history_items_omitted: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OriginTally {
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub items: u64,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub read_back: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryItem {
    #[serde(default)]
    pub origin: String,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub end: Option<String>,
}

/// Read trials from JSON Lines. Blank lines are skipped; a malformed line is an error naming it.
pub fn parse_trials(text: &str) -> Result<Vec<Trial>, String> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect()
}

/// One variant's trials of one scenario, summarized. Rates and means are per trial.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Summary {
    pub scenario: String,
    pub variant: String,
    pub trials: u64,
    pub succeeded: u64,
    pub errors: u64,
    pub success_rate: f64,
    pub sent_chars: f64,
    pub calls: f64,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_tokens: f64,
    pub regret: f64,
    pub waste_tokens: f64,
    pub cache_loss_tokens: f64,
    /// Share of recalled records the model read in full; `None` when nothing was recalled.
    pub recall_hit_rate: Option<f64>,
    /// `read_result` reads of earlier tool results or dependency outputs.
    pub read_back: f64,
    /// History items compaction archived.
    pub compacted_items: f64,
    /// Mean of |estimated − observed| / observed over every request with a provider count.
    pub estimate_error: Option<f64>,
    /// The worst request's estimate error.
    pub estimate_error_max: Option<f64>,
}

impl Summary {
    /// The value of a metric by name, for gates and tables.
    pub fn metric(&self, name: &str) -> Option<f64> {
        Some(match name {
            "success_rate" => self.success_rate,
            "sent_chars" => self.sent_chars,
            "calls" => self.calls,
            "input_tokens" => self.input_tokens,
            "output_tokens" => self.output_tokens,
            "cache_read_tokens" => self.cache_read_tokens,
            "regret" => self.regret,
            "waste_tokens" => self.waste_tokens,
            "cache_loss_tokens" => self.cache_loss_tokens,
            "recall_hit_rate" => return self.recall_hit_rate,
            "read_back" => self.read_back,
            "compacted_items" => self.compacted_items,
            "estimate_error" => return self.estimate_error,
            "estimate_error_max" => return self.estimate_error_max,
            _ => return None,
        })
    }
}

/// Metrics where a higher value is better; every other metric is better lower.
pub const HIGHER_IS_BETTER: &[&str] = &["success_rate", "recall_hit_rate"];

/// The metrics a report shows, in order.
pub const REPORT_METRICS: &[&str] = &[
    "success_rate",
    "sent_chars",
    "calls",
    "input_tokens",
    "output_tokens",
    "regret",
    "waste_tokens",
    "cache_loss_tokens",
    "recall_hit_rate",
    "read_back",
    "compacted_items",
    "estimate_error",
];

/// Summarize every (scenario, variant) pair, ordered by scenario then variant.
pub fn summarize(trials: &[Trial]) -> Vec<Summary> {
    let mut groups: BTreeMap<(&str, &str), Vec<&Trial>> = BTreeMap::new();
    for trial in trials {
        groups
            .entry((trial.scenario.as_str(), trial.variant.as_str()))
            .or_default()
            .push(trial);
    }
    groups
        .into_iter()
        .map(|((scenario, variant), trials)| summarize_group(scenario, variant, &trials))
        .collect()
}

fn summarize_group(scenario: &str, variant: &str, trials: &[&Trial]) -> Summary {
    let n = trials.len() as f64;
    let mean = |value: &dyn Fn(&Trial) -> u64| trials.iter().map(|t| value(t) as f64).sum::<f64>() / n;
    let reports = || trials.iter().flat_map(|trial| trial.lifecycle.iter());
    let sum_reports = |value: &dyn Fn(&LifecycleReport) -> u64| {
        reports().map(|report| value(report) as f64).sum::<f64>() / n
    };
    let recalled: u64 = reports().map(|report| report.recalled).sum();
    let expanded: u64 = reports().map(|report| report.recall_expanded).sum();
    let errors: Vec<f64> = trials
        .iter()
        .flat_map(|trial| trial.token_estimates.iter())
        .filter(|estimate| estimate.observed > 0)
        .map(|estimate| {
            (estimate.estimated as f64 - estimate.observed as f64).abs() / estimate.observed as f64
        })
        .collect();
    let succeeded = trials.iter().filter(|trial| trial.success).count() as u64;
    Summary {
        scenario: scenario.to_string(),
        variant: variant.to_string(),
        trials: trials.len() as u64,
        succeeded,
        errors: trials.iter().filter(|trial| trial.error.is_some()).count() as u64,
        success_rate: succeeded as f64 / n,
        sent_chars: mean(&|t| t.cost.sent_chars),
        calls: mean(&|t| t.cost.calls),
        input_tokens: mean(&|t| t.cost.input_tokens),
        output_tokens: mean(&|t| t.cost.output_tokens),
        cache_read_tokens: mean(&|t| t.cost.cache_read_tokens),
        regret: sum_reports(&|r| r.regret),
        waste_tokens: sum_reports(&|r| r.waste_tokens),
        cache_loss_tokens: sum_reports(&|r| r.cache_loss_tokens),
        recall_hit_rate: (recalled > 0).then(|| expanded as f64 / recalled as f64),
        read_back: sum_reports(&|r| r.history.iter().map(|tally| tally.read_back).sum()),
        compacted_items: sum_reports(&|r| {
            r.history_items
                .iter()
                .filter(|item| item.end.as_deref() == Some("compacted"))
                .count() as u64
        }),
        estimate_error: (!errors.is_empty())
            .then(|| errors.iter().sum::<f64>() / errors.len() as f64),
        estimate_error_max: errors.iter().copied().reduce(f64::max),
    }
}

/// A check of a candidate variant against a baseline variant of the same scenario.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gate {
    /// Metrics the candidate may not be worse on (direction from [`HIGHER_IS_BETTER`]).
    pub no_worse: Vec<String>,
    /// Upper bound on the candidate's mean estimate error (0.10 = 10%).
    pub max_estimate_error: Option<f64>,
}

/// One failed check.
#[derive(Debug, Clone, PartialEq)]
pub struct GateFailure {
    pub scenario: String,
    pub metric: String,
    pub detail: String,
}

impl Gate {
    /// Check `candidate` against `baseline` in every scenario both were run in. A scenario only
    /// one of them ran in is reported as a failure, so a gate cannot pass vacuously.
    pub fn check(&self, summaries: &[Summary], baseline: &str, candidate: &str) -> Vec<GateFailure> {
        let find = |scenario: &str, variant: &str| {
            summaries
                .iter()
                .find(|s| s.scenario == scenario && s.variant == variant)
        };
        let mut scenarios: Vec<&str> = summaries
            .iter()
            .filter(|s| s.variant == baseline || s.variant == candidate)
            .map(|s| s.scenario.as_str())
            .collect();
        scenarios.dedup();
        if scenarios.is_empty() {
            return vec![GateFailure {
                scenario: String::new(),
                metric: String::new(),
                detail: format!("no trials of {baseline:?} or {candidate:?}"),
            }];
        }
        let mut failures = Vec::new();
        for scenario in scenarios {
            let fail = |metric: &str, detail: String| GateFailure {
                scenario: scenario.to_string(),
                metric: metric.to_string(),
                detail,
            };
            let (Some(base), Some(cand)) = (find(scenario, baseline), find(scenario, candidate))
            else {
                failures.push(fail("", format!("{baseline:?} and {candidate:?} were not both run")));
                continue;
            };
            for metric in &self.no_worse {
                let (Some(b), Some(c)) = (base.metric(metric), cand.metric(metric)) else {
                    if base.metric(metric).is_some() != cand.metric(metric).is_some() {
                        failures.push(fail(metric, "measured for only one variant".to_string()));
                    }
                    continue;
                };
                let worse = if HIGHER_IS_BETTER.contains(&metric.as_str()) {
                    c < b
                } else {
                    c > b
                };
                if worse {
                    failures.push(fail(metric, format!("{c:.4} vs baseline {b:.4}")));
                }
            }
            if let Some(bound) = self.max_estimate_error {
                match cand.estimate_error {
                    Some(error) if error > bound => failures.push(fail(
                        "estimate_error",
                        format!("{:.1}% exceeds {:.1}%", error * 100.0, bound * 100.0),
                    )),
                    Some(_) => {}
                    None => failures.push(fail(
                        "estimate_error",
                        "no request carried both an estimate and a provider count".to_string(),
                    )),
                }
            }
        }
        failures
    }
}

/// A Markdown table per scenario: one column per variant, and a change column against `baseline`
/// for every other variant.
pub fn markdown(summaries: &[Summary], baseline: Option<&str>) -> String {
    let mut scenarios: Vec<&str> = summaries.iter().map(|s| s.scenario.as_str()).collect();
    scenarios.dedup();
    let mut out = String::new();
    for scenario in scenarios {
        let mut columns: Vec<&Summary> =
            summaries.iter().filter(|s| s.scenario == scenario).collect();
        if let Some(name) = baseline {
            columns.sort_by_key(|s| s.variant != name);
        }
        let base = baseline.and_then(|name| columns.iter().find(|s| s.variant == name).copied());
        out.push_str(&format!("### {scenario}\n\n| metric |"));
        for column in &columns {
            out.push_str(&format!(" {} |", column.variant));
            if base.is_some_and(|b| b.variant != column.variant) {
                out.push_str(" Δ |");
            }
        }
        out.push_str("\n|---|");
        for column in &columns {
            out.push_str("---|");
            if base.is_some_and(|b| b.variant != column.variant) {
                out.push_str("---|");
            }
        }
        out.push('\n');
        let trials_row = columns.iter().fold(String::from("| trials |"), |row, s| {
            let extra = if base.is_some_and(|b| b.variant != s.variant) { " |" } else { "" };
            format!("{row} {} ({} errors) |{extra}", s.trials, s.errors)
        });
        out.push_str(&trials_row);
        out.push('\n');
        for metric in REPORT_METRICS {
            // A count that stayed zero everywhere says nothing; a rate that was measured does.
            let shown = |value: Option<f64>| match value {
                Some(v) => v != 0.0 || is_ratio(metric),
                None => false,
            };
            if !columns.iter().any(|s| shown(s.metric(metric))) {
                continue;
            }
            out.push_str(&format!("| {metric} |"));
            for column in &columns {
                let value = column.metric(metric);
                out.push_str(&format!(" {} |", format_metric(metric, value)));
                if let Some(b) = base.filter(|b| b.variant != column.variant) {
                    out.push_str(&format!(" {} |", format_delta(metric, b.metric(metric), value)));
                }
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn is_ratio(metric: &str) -> bool {
    metric.ends_with("_rate") || metric.starts_with("estimate_error")
}

fn format_metric(metric: &str, value: Option<f64>) -> String {
    match value {
        None => "–".to_string(),
        Some(v) if is_ratio(metric) => {
            format!("{:.1}%", v * 100.0)
        }
        Some(v) if v.fract() == 0.0 || v.abs() >= 100.0 => format!("{v:.0}"),
        Some(v) => format!("{v:.2}"),
    }
}

fn format_delta(metric: &str, base: Option<f64>, value: Option<f64>) -> String {
    let (Some(b), Some(v)) = (base, value) else {
        return "–".to_string();
    };
    if is_ratio(metric) {
        return format!("{:+.1} pt", (v - b) * 100.0);
    }
    if b == 0.0 {
        return if v == 0.0 { "0".to_string() } else { format!("{:+.2}", v - b) };
    }
    format!("{:+.1}%", (v - b) / b * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trial(variant: &str, task: &str, success: bool, regret: u64, chars: u64) -> Trial {
        Trial {
            scenario: "long-task".into(),
            variant: variant.into(),
            task: task.into(),
            success,
            cost: Cost { sent_chars: chars, calls: 2, ..Cost::default() },
            token_estimates: vec![TokenEstimate { estimated: 105, observed: 100 }],
            lifecycle: vec![LifecycleReport {
                regret,
                waste_tokens: 10 * regret,
                recalled: 2,
                recall_expanded: 1,
                ..LifecycleReport::default()
            }],
            ..Trial::default()
        }
    }

    #[test]
    fn a_trial_from_an_older_checkout_reads_with_defaults() {
        let line = r#"{"scenario":"s","variant":"v","task":"t","success":true,"lifecycle":[{"regret":1,"knowledge_entries":2,"turn":4}],"extra":1}"#;
        let trials = parse_trials(&format!("{line}\n\n")).unwrap();
        assert_eq!(trials.len(), 1);
        assert_eq!(trials[0].lifecycle[0].regret, 1);
        assert!(trials[0].lifecycle[0].history_items.is_empty());
        assert!(parse_trials("{").unwrap_err().starts_with("line 1:"));
    }

    #[test]
    fn variants_are_summarized_per_trial() {
        let summaries = summarize(&[
            trial("base", "t0", true, 1, 1000),
            trial("base", "t1", false, 3, 3000),
            trial("cand", "t0", true, 0, 800),
        ]);
        let base = &summaries[0];
        assert_eq!((base.variant.as_str(), base.trials, base.succeeded), ("base", 2, 1));
        assert_eq!(base.success_rate, 0.5);
        assert_eq!(base.regret, 2.0);
        assert_eq!(base.sent_chars, 2000.0);
        assert_eq!(base.recall_hit_rate, Some(0.5));
        assert!((base.estimate_error.unwrap() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn a_gate_fails_on_any_regression_and_on_a_missing_side() {
        let summaries = summarize(&[
            trial("base", "t0", true, 1, 1000),
            trial("cand", "t0", true, 2, 800),
        ]);
        let gate = Gate {
            no_worse: vec!["regret".into(), "success_rate".into(), "sent_chars".into()],
            max_estimate_error: Some(0.10),
        };
        let failures = gate.check(&summaries, "base", "cand");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].metric, "regret");

        let strict = Gate { no_worse: vec![], max_estimate_error: Some(0.01) };
        assert_eq!(strict.check(&summaries, "base", "cand")[0].metric, "estimate_error");
        assert!(!gate.check(&summaries, "base", "missing").is_empty());
    }

    #[test]
    fn the_markdown_report_shows_changes_against_the_baseline() {
        let summaries = summarize(&[
            trial("base", "t0", true, 2, 1000),
            trial("cand", "t0", true, 1, 800),
        ]);
        let table = markdown(&summaries, Some("base"));
        assert!(table.contains("| metric | base | cand | Δ |"), "{table}");
        assert!(table.contains("| sent_chars | 1000 | 800 | -20.0% |"), "{table}");
        assert!(table.contains("| regret | 2 | 1 | -50.0% |"), "{table}");
        assert!(table.contains("| recall_hit_rate | 50.0% | 50.0% | +0.0 pt |"), "{table}");
        assert!(!table.contains("cache_loss_tokens"), "an all-zero count is left out: {table}");
    }
}
