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
    /// The model that ran the task, for pricing (plan task 8.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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
    /// Ground truth only the scenario knows (which items the answer needed, which were noise),
    /// as named counts. The kernel's own regret and waste rest on the evidence it gathered; these
    /// do not, so a change to that evidence cannot grade itself. Read as `oracle.<name>`, lower is
    /// better.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub oracle: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// Prompt tokens written to cache, summed (a subset of `input_tokens`, like cache reads).
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Plan task 8.0 · each provider request's usage, in the order sent. Absent on trials
    /// recorded before 0.2.82.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<RequestCost>,
}

/// One provider request's usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestCost {
    /// The run that sent it, when a trial has several (a workflow's nodes); requests are compared
    /// with the previous request of the same run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// Plan task 8.1 · price per token of each class. `input_tokens` counts the whole prompt, so the
/// uncached part is `input − cache_read − cache_write`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Prices {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

impl Default for Prices {
    /// Relative units (uncached input = 1), not real prices.
    fn default() -> Self {
        Self { input: 1.0, cache_read: 0.1, cache_write: 1.0, output: 4.0 }
    }
}

/// Prices by model, with a fallback. Read from JSON: `{ "default": {..}, "<model>": {..} }`; keys
/// starting with `_` are comments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PriceTable {
    pub default: Prices,
    pub models: BTreeMap<String, Prices>,
}

impl PriceTable {
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        let mut table = PriceTable::default();
        for (key, value) in raw {
            if key.starts_with('_') {
                continue;
            }
            let prices: Prices =
                serde_json::from_value(value).map_err(|error| format!("{key}: {error}"))?;
            if key == "default" {
                table.default = prices;
            } else {
                table.models.insert(key, prices);
            }
        }
        Ok(table)
    }

    pub fn for_model(&self, model: Option<&str>) -> Prices {
        model.and_then(|model| self.models.get(model)).copied().unwrap_or(self.default)
    }
}

/// Plan task 8.1 · what the trial cost at `prices`.
pub fn weighted_cost(trial: &Trial, prices: &PriceTable) -> f64 {
    let p = prices.for_model(trial.model.as_deref());
    let c = &trial.cost;
    let uncached = c.input_tokens.saturating_sub(c.cache_read_tokens + c.cache_write_tokens);
    uncached as f64 * p.input
        + c.cache_read_tokens as f64 * p.cache_read
        + c.cache_write_tokens as f64 * p.cache_write
        + c.output_tokens as f64 * p.output
}

/// Plan task 8.0 · what the provider says the trial lost to broken prefixes: for each request
/// after the first of its run, the previous request's input that was not read from cache
/// (`max(0, previous input − cache read)`), less the kernel's `cache_tail_tokens` (the state turns,
/// which are rebuilt every request and could never be reused). `None` without request detail.
pub fn observed_cache_loss(trial: &Trial) -> Option<u64> {
    let requests = &trial.cost.requests;
    if requests.len() < 2 {
        return None;
    }
    let mut previous: BTreeMap<Option<&str>, u64> = BTreeMap::new();
    let mut missed = 0u64;
    for request in requests {
        let run = request.run.as_deref();
        if let Some(prev_input) = previous.insert(run, request.input_tokens) {
            missed += prev_input.saturating_sub(request.cache_read_tokens);
        }
    }
    let tail: u64 = trial.lifecycle.iter().map(|report| report.cache_tail_tokens).sum();
    Some(missed.saturating_sub(tail))
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
    /// Plan task 8.0 · state-turn tokens that were never reusable (not a break).
    #[serde(default)]
    pub cache_tail_tokens: u64,
    /// Plan task 8.0 · the latest prefix breaks, attributed.
    #[serde(default)]
    pub prefix_breaks: Vec<PrefixBreak>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PrefixBreak {
    #[serde(default)]
    pub cause: String,
    #[serde(default)]
    pub lost_tokens: u64,
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
    /// Plan task 8.1 · mean [`weighted_cost`] per trial: the primary cost metric.
    pub weighted_cost: f64,
    pub sent_chars: f64,
    pub calls: f64,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub cache_read_tokens: f64,
    pub regret: f64,
    pub waste_tokens: f64,
    pub cache_loss_tokens: f64,
    /// Plan task 8.0 · the provider-measured cache loss per trial ([`observed_cache_loss`]), over
    /// the trials with request detail.
    pub observed_cache_loss_tokens: Option<f64>,
    /// |Σ kernel `cache_loss_tokens` − Σ observed| / Σ observed over the trials with request
    /// detail: how far the kernel's account is from the provider's.
    pub cache_loss_error: Option<f64>,
    /// Kernel-reported lost tokens per trial by break cause (from the listed breaks). Read as
    /// `cache_loss.<cause>`.
    pub cache_loss_by_cause: BTreeMap<String, f64>,
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
    /// Σ|estimated − observed| / Σ observed: the error weighted by request size, which is what
    /// compaction and renewal decisions feel (a small first request barely matters to them).
    pub estimate_error_weighted: Option<f64>,
    /// Mean of each oracle count per trial, over the trials that report it.
    pub oracle: BTreeMap<String, f64>,
}

impl Summary {
    /// The value of a metric by name, for gates and tables.
    pub fn metric(&self, name: &str) -> Option<f64> {
        Some(match name {
            "success_rate" => self.success_rate,
            "weighted_cost" => self.weighted_cost,
            "sent_chars" => self.sent_chars,
            "calls" => self.calls,
            "input_tokens" => self.input_tokens,
            "output_tokens" => self.output_tokens,
            "cache_read_tokens" => self.cache_read_tokens,
            "regret" => self.regret,
            "waste_tokens" => self.waste_tokens,
            "cache_loss_tokens" => self.cache_loss_tokens,
            "observed_cache_loss_tokens" => return self.observed_cache_loss_tokens,
            "cache_loss_error" => return self.cache_loss_error,
            "recall_hit_rate" => return self.recall_hit_rate,
            "read_back" => self.read_back,
            "compacted_items" => self.compacted_items,
            "estimate_error" => return self.estimate_error,
            "estimate_error_max" => return self.estimate_error_max,
            "estimate_error_weighted" => return self.estimate_error_weighted,
            name => {
                if let Some(cause) = name.strip_prefix("cache_loss.") {
                    return self.cache_loss_by_cause.get(cause).copied();
                }
                return name.strip_prefix("oracle.").and_then(|key| self.oracle.get(key).copied());
            }
        })
    }
}

/// Metrics where a higher value is better; every other metric is better lower.
pub const HIGHER_IS_BETTER: &[&str] = &["success_rate", "recall_hit_rate"];

/// The metrics a report shows, in order.
pub const REPORT_METRICS: &[&str] = &[
    "success_rate",
    "weighted_cost",
    "sent_chars",
    "calls",
    "input_tokens",
    "output_tokens",
    "regret",
    "waste_tokens",
    "cache_loss_tokens",
    "observed_cache_loss_tokens",
    "cache_loss_error",
    "recall_hit_rate",
    "read_back",
    "compacted_items",
    "estimate_error",
    "estimate_error_weighted",
];

/// Summarize every (scenario, variant) pair at the default (relative) prices.
pub fn summarize(trials: &[Trial]) -> Vec<Summary> {
    summarize_with(trials, &PriceTable::default())
}

/// Summarize every (scenario, variant) pair, ordered by scenario then variant.
pub fn summarize_with(trials: &[Trial], prices: &PriceTable) -> Vec<Summary> {
    let mut groups: BTreeMap<(&str, &str), Vec<&Trial>> = BTreeMap::new();
    for trial in trials {
        groups
            .entry((trial.scenario.as_str(), trial.variant.as_str()))
            .or_default()
            .push(trial);
    }
    groups
        .into_iter()
        .map(|((scenario, variant), trials)| summarize_group(scenario, variant, &trials, prices))
        .collect()
}

fn summarize_group(scenario: &str, variant: &str, trials: &[&Trial], prices: &PriceTable) -> Summary {
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
    let counted: Vec<&TokenEstimate> = trials
        .iter()
        .flat_map(|trial| trial.token_estimates.iter())
        .filter(|estimate| estimate.observed > 0)
        .collect();
    let observed_total: f64 = counted.iter().map(|e| e.observed as f64).sum();
    let error_total: f64 = counted
        .iter()
        .map(|e| (e.estimated as f64 - e.observed as f64).abs())
        .sum();
    let succeeded = trials.iter().filter(|trial| trial.success).count() as u64;
    let measured: Vec<(u64, u64)> = trials
        .iter()
        .filter_map(|trial| {
            let kernel = trial.lifecycle.iter().map(|report| report.cache_loss_tokens).sum();
            observed_cache_loss(trial).map(|observed| (kernel, observed))
        })
        .collect();
    let observed_sum: u64 = measured.iter().map(|(_, observed)| observed).sum();
    let kernel_sum: u64 = measured.iter().map(|(kernel, _)| kernel).sum();
    let mut by_cause: BTreeMap<String, f64> = BTreeMap::new();
    for brk in reports().flat_map(|report| report.prefix_breaks.iter()) {
        *by_cause.entry(brk.cause.clone()).or_default() += brk.lost_tokens as f64 / n;
    }
    let mut oracle_sums: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for (key, value) in trials.iter().flat_map(|trial| trial.oracle.iter()) {
        let (total, count) = oracle_sums.entry(key.clone()).or_default();
        *total += value;
        *count += 1.0;
    }
    Summary {
        scenario: scenario.to_string(),
        variant: variant.to_string(),
        trials: trials.len() as u64,
        succeeded,
        errors: trials.iter().filter(|trial| trial.error.is_some()).count() as u64,
        success_rate: succeeded as f64 / n,
        weighted_cost: trials.iter().map(|trial| weighted_cost(trial, prices)).sum::<f64>() / n,
        sent_chars: mean(&|t| t.cost.sent_chars),
        calls: mean(&|t| t.cost.calls),
        input_tokens: mean(&|t| t.cost.input_tokens),
        output_tokens: mean(&|t| t.cost.output_tokens),
        cache_read_tokens: mean(&|t| t.cost.cache_read_tokens),
        regret: sum_reports(&|r| r.regret),
        waste_tokens: sum_reports(&|r| r.waste_tokens),
        cache_loss_tokens: sum_reports(&|r| r.cache_loss_tokens),
        observed_cache_loss_tokens: (!measured.is_empty())
            .then(|| observed_sum as f64 / measured.len() as f64),
        cache_loss_error: match (measured.is_empty(), observed_sum) {
            (true, _) => None,
            (false, 0) => Some(if kernel_sum == 0 { 0.0 } else { 1.0 }),
            (false, observed) => Some((kernel_sum as f64 - observed as f64).abs() / observed as f64),
        },
        cache_loss_by_cause: by_cause,
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
        estimate_error_weighted: (observed_total > 0.0).then(|| error_total / observed_total),
        oracle: oracle_sums
            .into_iter()
            .map(|(key, (total, count))| (key, total / count))
            .collect(),
    }
}

/// A check of a candidate variant against a baseline variant of the same scenario.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gate {
    /// Metrics the candidate may not be worse on (direction from [`HIGHER_IS_BETTER`]).
    pub no_worse: Vec<String>,
    /// Upper bound on the candidate's size-weighted estimate error (0.10 = 10%).
    pub max_estimate_error: Option<f64>,
    /// Plan task 8.0 · upper bound on the candidate's [`Summary::cache_loss_error`].
    pub max_cache_loss_error: Option<f64>,
    /// Plan task 8.1 · metrics compared task by task, with a bootstrap confidence interval.
    pub paired: Vec<PairedCheck>,
    /// Fewer pairs than this and a paired check fails as "insufficient samples".
    pub min_pairs: usize,
    /// DEC-13 · in every scenario, at least one of these metrics must be significantly better
    /// (its whole 95% interval on the better side of zero). A mechanism earns its place by making
    /// something better; "not shown to be worse" is not enough. Empty: no such requirement.
    pub improve: Vec<String>,
}

/// Plan task 8.1 · a paired check: the candidate is "no worse" on `metric` only if the 95%
/// interval of its per-task difference from the baseline stays within `tolerance` on the worse
/// side. For a metric where higher is better (success) the tolerance is absolute: the lower bound
/// of `candidate − baseline` must be ≥ −tolerance (0.05 = five points). For any other metric it is
/// relative to the baseline mean: the upper bound of `(candidate − baseline) / mean(baseline)` must
/// be ≤ tolerance (0.0 = not significantly more expensive).
#[derive(Debug, Clone, PartialEq)]
pub struct PairedCheck {
    pub metric: String,
    pub tolerance: f64,
    /// `metric:ns` · DEC-13 · fail only when the candidate is significantly worse: the whole 95%
    /// interval lies on the worse side of zero. The tolerance is unused.
    pub not_significantly_worse: bool,
}

impl PairedCheck {
    /// `metric:tolerance`, e.g. `success_rate:0.05`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (metric, tolerance) = text
            .split_once(':')
            .ok_or_else(|| format!("{text:?}: expected <metric>:<tolerance> or <metric>:ns"))?;
        let metric = metric.trim().to_string();
        if tolerance.trim() == "ns" {
            return Ok(Self { metric, tolerance: 0.0, not_significantly_worse: true });
        }
        let tolerance = tolerance.trim().parse::<f64>().map_err(|e| format!("{text:?}: {e}"))?;
        Ok(Self { metric, tolerance, not_significantly_worse: false })
    }
}

/// Default for [`Gate::min_pairs`].
pub const MIN_PAIRS: usize = 30;

/// DEC-13 · the paired checks `gate` runs when `--paired` is not given: the candidate may be
/// neither significantly less successful nor significantly more expensive per correct answer.
/// Together with [`DEFAULT_IMPROVE`], a change passes only if it makes one of the two significantly
/// better and neither significantly worse.
///
/// Changed after the 0.2.82 measurements, and said so. The earlier default
/// (`success_rate:0.05,weighted_cost:0`, plan task 8.1) was written for changes meant to save cost:
/// it asked cost to be significantly *lower*. Notes (plan task 9.1) raised success by 17–50 points
/// and cut cost per correct answer by 43–68% where it was significant, yet failed that default in
/// every scenario on one side or the other. What a change is for is getting answers right at a
/// price; `cost_per_success` measures exactly that.
pub const DEFAULT_PAIRED: &str = "success_rate:ns,cost_per_success:ns";

/// DEC-13 · the metrics of which `gate` requires at least one to be significantly better when
/// `--improve` is not given. Without it, "not significantly worse" passes any small sample: 8.3b
/// (long-horizon 12/30 → 8/30) would have passed.
pub const DEFAULT_IMPROVE: &str = "success_rate,cost_per_success";

/// One task's value of a metric, for pairing. `None` when the trial does not measure it.
pub fn trial_metric(trial: &Trial, metric: &str, prices: &PriceTable) -> Option<f64> {
    let sum = |value: &dyn Fn(&LifecycleReport) -> u64| {
        trial.lifecycle.iter().map(|report| value(report) as f64).sum::<f64>()
    };
    Some(match metric {
        "success_rate" => f64::from(u8::from(trial.success)),
        "weighted_cost" => weighted_cost(trial, prices),
        "sent_chars" => trial.cost.sent_chars as f64,
        "calls" => trial.cost.calls as f64,
        "input_tokens" => trial.cost.input_tokens as f64,
        "output_tokens" => trial.cost.output_tokens as f64,
        "cache_read_tokens" => trial.cost.cache_read_tokens as f64,
        "regret" => sum(&|r| r.regret),
        "waste_tokens" => sum(&|r| r.waste_tokens),
        "cache_loss_tokens" => sum(&|r| r.cache_loss_tokens),
        "observed_cache_loss_tokens" => observed_cache_loss(trial)? as f64,
        name => return name.strip_prefix("oracle.").and_then(|key| trial.oracle.get(key).copied()),
    })
}

/// Pair the two variants' trials of `scenario` by task: the k-th run of a task under one variant
/// with its k-th run under the other. Unpaired runs are left out.
pub fn pair_trials<'a>(
    trials: &'a [Trial],
    scenario: &str,
    baseline: &str,
    candidate: &str,
) -> Vec<(&'a Trial, &'a Trial)> {
    let runs = |variant: &str| {
        let mut by_task: BTreeMap<&str, Vec<&'a Trial>> = BTreeMap::new();
        for trial in trials.iter().filter(|t| t.scenario == scenario && t.variant == variant) {
            by_task.entry(trial.task.as_str()).or_default().push(trial);
        }
        by_task
    };
    let (base, cand) = (runs(baseline), runs(candidate));
    base.iter()
        .filter_map(|(task, base_runs)| cand.get(task).map(|cand_runs| (base_runs, cand_runs)))
        .flat_map(|(base_runs, cand_runs)| base_runs.iter().copied().zip(cand_runs.iter().copied()))
        .collect()
}

/// The 95% percentile-bootstrap interval of the mean of `diffs`, from a fixed seed so the same
/// trials always give the same interval.
pub fn bootstrap_interval(diffs: &[f64]) -> (f64, f64) {
    const RESAMPLES: usize = 2000;
    if diffs.is_empty() {
        return (0.0, 0.0);
    }
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let n = diffs.len();
    let mut means: Vec<f64> = (0..RESAMPLES)
        .map(|_| (0..n).map(|_| diffs[(next() % n as u64) as usize]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let at = |q: f64| means[((RESAMPLES as f64 - 1.0) * q).round() as usize];
    (at(0.025), at(0.975))
}

/// DEC-13 · price-weighted cost per correct answer, a scenario-level ratio.
pub const COST_PER_SUCCESS: &str = "cost_per_success";

/// `cost_per_success` compared over pairs: the relative change of Σcost / Σsuccesses, with a
/// fixed-seed bootstrap over the pairs. A resample in which either side answered nothing is
/// skipped (its ratio is undefined); with no successes at all the interval is unbounded upward.
pub fn cost_per_success_result(pairs: &[(&Trial, &Trial)], prices: &PriceTable) -> PairedResult {
    const RESAMPLES: usize = 2000;
    let rows: Vec<(f64, f64, f64, f64)> = pairs
        .iter()
        .map(|(b, c)| {
            (
                weighted_cost(b, prices),
                f64::from(u8::from(b.success)),
                weighted_cost(c, prices),
                f64::from(u8::from(c.success)),
            )
        })
        .collect();
    let ratio = |index: &mut dyn Iterator<Item = usize>| -> Option<f64> {
        let (mut bc, mut bs, mut cc, mut cs) = (0.0, 0.0, 0.0, 0.0);
        for i in index {
            let (a, b, c, d) = rows[i];
            bc += a;
            bs += b;
            cc += c;
            cs += d;
        }
        (bs > 0.0 && cs > 0.0 && bc > 0.0).then(|| (cc / cs) / (bc / bs) - 1.0)
    };
    let n = rows.len();
    let Some(mean) = ratio(&mut (0..n)) else {
        return PairedResult { pairs: n, mean: f64::INFINITY, low: f64::INFINITY, high: f64::INFINITY };
    };
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut samples: Vec<f64> = (0..RESAMPLES)
        .filter_map(|_| ratio(&mut (0..n).map(|_| (next() % n as u64) as usize).collect::<Vec<_>>().into_iter()))
        .collect();
    if samples.is_empty() {
        return PairedResult { pairs: n, mean, low: mean, high: mean };
    }
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize];
    PairedResult { pairs: n, mean, low: at(0.025), high: at(0.975) }
}

/// The result of one paired check in one scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedResult {
    pub pairs: usize,
    /// Mean per-task difference (relative to the baseline mean for lower-is-better metrics).
    pub mean: f64,
    pub low: f64,
    pub high: f64,
}

/// Compare `metric` task by task. Differences are `candidate − baseline`; for a lower-is-better
/// metric they are divided by the baseline mean, so the interval reads as a relative change.
pub fn paired_result(pairs: &[(&Trial, &Trial)], metric: &str, prices: &PriceTable) -> PairedResult {
    if metric == COST_PER_SUCCESS {
        return cost_per_success_result(pairs, prices);
    }
    let values: Vec<(f64, f64)> = pairs
        .iter()
        .filter_map(|(b, c)| Some((trial_metric(b, metric, prices)?, trial_metric(c, metric, prices)?)))
        .collect();
    let base_mean = values.iter().map(|(b, _)| b).sum::<f64>() / values.len().max(1) as f64;
    let scale = if HIGHER_IS_BETTER.contains(&metric) || base_mean == 0.0 { 1.0 } else { base_mean };
    let diffs: Vec<f64> = values.iter().map(|(b, c)| (c - b) / scale).collect();
    let (low, high) = bootstrap_interval(&diffs);
    PairedResult {
        pairs: diffs.len(),
        mean: diffs.iter().sum::<f64>() / diffs.len().max(1) as f64,
        low,
        high,
    }
}

impl Gate {
    /// The summary checks of [`Gate::check`] plus the paired checks, which need the trials.
    pub fn check_trials(
        &self,
        trials: &[Trial],
        prices: &PriceTable,
        baseline: &str,
        candidate: &str,
    ) -> Vec<GateFailure> {
        let summaries = summarize_with(trials, prices);
        let mut failures = self.check(&summaries, baseline, candidate);
        if self.paired.is_empty() && self.improve.is_empty() {
            return failures;
        }
        let mut scenarios: Vec<&str> = summaries
            .iter()
            .filter(|s| s.variant == baseline || s.variant == candidate)
            .map(|s| s.scenario.as_str())
            .collect();
        scenarios.dedup();
        let min_pairs = if self.min_pairs == 0 { MIN_PAIRS } else { self.min_pairs };
        for scenario in scenarios {
            let pairs = pair_trials(trials, scenario, baseline, candidate);
            if !self.improve.is_empty() {
                let improved = self.improve.iter().any(|metric| {
                    let result = paired_result(&pairs, metric, prices);
                    if HIGHER_IS_BETTER.contains(&metric.as_str()) { result.low > 0.0 } else { result.high < 0.0 }
                });
                if pairs.len() >= min_pairs && !improved {
                    failures.push(GateFailure {
                        scenario: scenario.to_string(),
                        metric: format!("improve {}", self.improve.join("|")),
                        detail: "none of these is significantly better".to_string(),
                    });
                }
            }
            for check in &self.paired {
                let result = paired_result(&pairs, &check.metric, prices);
                let fail = |detail: String| GateFailure {
                    scenario: scenario.to_string(),
                    metric: format!("paired {}", check.metric),
                    detail,
                };
                if result.pairs < min_pairs {
                    failures.push(fail(format!(
                        "insufficient samples: {} pairs, need {min_pairs}",
                        result.pairs
                    )));
                    continue;
                }
                let higher_better = HIGHER_IS_BETTER.contains(&check.metric.as_str());
                let interval = format!(
                    "mean {:+.4}, 95% CI [{:+.4}, {:+.4}] over {} pairs",
                    result.mean, result.low, result.high, result.pairs
                );
                if check.not_significantly_worse {
                    if higher_better && result.high < 0.0 {
                        failures.push(fail(format!("{interval}: significantly lower")));
                    } else if !higher_better && result.low > 0.0 {
                        failures.push(fail(format!("{interval}: significantly higher")));
                    }
                } else if higher_better && result.low < -check.tolerance {
                    failures.push(fail(format!("{interval}: lower bound below -{}", check.tolerance)));
                } else if !higher_better && result.high > check.tolerance {
                    failures.push(fail(format!("{interval}: upper bound above +{}", check.tolerance)));
                }
            }
        }
        failures
    }
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
                match cand.estimate_error_weighted {
                    Some(error) if error > bound => failures.push(fail(
                        "estimate_error_weighted",
                        format!("{:.1}% exceeds {:.1}%", error * 100.0, bound * 100.0),
                    )),
                    Some(_) => {}
                    None => failures.push(fail(
                        "estimate_error_weighted",
                        "no request carried both an estimate and a provider count".to_string(),
                    )),
                }
            }
            if let Some(bound) = self.max_cache_loss_error {
                match cand.cache_loss_error {
                    Some(error) if error > bound => failures.push(fail(
                        "cache_loss_error",
                        format!("{:.1}% exceeds {:.1}%", error * 100.0, bound * 100.0),
                    )),
                    Some(_) => {}
                    None => failures.push(fail(
                        "cache_loss_error",
                        "no trial carried per-request usage".to_string(),
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
        let oracle_metrics: std::collections::BTreeSet<String> = columns
            .iter()
            .flat_map(|s| {
                s.cache_loss_by_cause
                    .keys()
                    .map(|cause| format!("cache_loss.{cause}"))
                    .chain(s.oracle.keys().map(|key| format!("oracle.{key}")))
            })
            .collect();
        let metrics = REPORT_METRICS
            .iter()
            .map(|metric| metric.to_string())
            .chain(oracle_metrics);
        for metric in metrics {
            let metric = metric.as_str();
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
    metric.ends_with("_rate") || metric.starts_with("estimate_error") || metric == "cache_loss_error"
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
    fn oracle_counts_summarize_gate_and_report_by_name() {
        let with_oracle = |variant: &str, task: &str, dropped: f64| Trial {
            oracle: BTreeMap::from([("required_dropped".to_string(), dropped)]),
            ..trial(variant, task, true, 0, 100)
        };
        let trials = parse_trials(
            &[
                with_oracle("base", "a", 1.0),
                with_oracle("base", "b", 2.0),
                with_oracle("cand", "a", 2.0),
                with_oracle("cand", "b", 2.0),
            ]
            .iter()
            .map(|trial| serde_json::to_string(trial).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        let summaries = summarize(&trials);
        assert_eq!(summaries[0].metric("oracle.required_dropped"), Some(1.5));
        assert_eq!(summaries[0].metric("oracle.missing"), None);
        let gate = Gate { no_worse: vec!["oracle.required_dropped".into()], ..Gate::default() };
        let failures = gate.check(&summaries, "base", "cand");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].metric, "oracle.required_dropped");
        let table = markdown(&summaries, Some("base"));
        assert!(table.contains("| oracle.required_dropped | 1.50 | 2 | +33.3% |"), "{table}");
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
        assert!((base.estimate_error_weighted.unwrap() - 0.05).abs() < 1e-9);
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
            ..Gate::default()
        };
        let failures = gate.check(&summaries, "base", "cand");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].metric, "regret");

        let strict = Gate { max_estimate_error: Some(0.01), ..Gate::default() };
        assert_eq!(strict.check(&summaries, "base", "cand")[0].metric, "estimate_error_weighted");
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

    #[test]
    fn measured_cache_loss_is_compared_with_the_kernel_account() {
        let request = |run: Option<&str>, input: u64, read: u64| RequestCost {
            run: run.map(str::to_string),
            input_tokens: input,
            cache_read_tokens: read,
            ..RequestCost::default()
        };
        let cand = |kernel: u64| Trial {
            cost: Cost {
                requests: vec![
                    request(None, 1000, 0),
                    // Reused all of the first request but its 50-token state turn.
                    request(None, 1400, 950),
                    // A break: only 600 of the previous 1400 were read back.
                    request(None, 1500, 600),
                    // Another run's first request is not compared with this one's.
                    request(Some("node"), 900, 0),
                ],
                ..Cost::default()
            },
            lifecycle: vec![LifecycleReport {
                cache_loss_tokens: kernel,
                cache_tail_tokens: 100,
                prefix_breaks: vec![PrefixBreak { cause: "narration".into(), lost_tokens: kernel }],
                ..LifecycleReport::default()
            }],
            ..trial("cand", "t0", true, 0, 100)
        };
        // Missed: 50 + 800 = 850; less the 100 state-turn tokens = 750.
        assert_eq!(observed_cache_loss(&cand(780)), Some(750));
        assert_eq!(observed_cache_loss(&trial("base", "t0", true, 0, 100)), None);

        let summaries = summarize(&[trial("base", "t0", true, 0, 100), cand(780)]);
        let summary = &summaries[1];
        assert_eq!(summary.observed_cache_loss_tokens, Some(750.0));
        assert!((summary.cache_loss_error.unwrap() - 0.04).abs() < 1e-9);
        assert_eq!(summary.metric("cache_loss.narration"), Some(780.0));
        assert_eq!(summaries[0].cache_loss_error, None, "no request detail, no comparison");

        let gate = Gate { max_cache_loss_error: Some(0.15), ..Gate::default() };
        assert!(gate.check(&summaries, "base", "cand").is_empty());
        let strict = Gate { max_cache_loss_error: Some(0.01), ..Gate::default() };
        assert_eq!(strict.check(&summaries, "base", "cand")[0].metric, "cache_loss_error");
        assert!(!gate.check(&summaries, "cand", "base").is_empty(), "a candidate without detail fails");
        let table = markdown(&summaries, None);
        assert!(table.contains("| cache_loss_error | – | 4.0% |"), "{table}");
        assert!(table.contains("| cache_loss.narration |"), "{table}");
    }

    #[test]
    fn weighted_cost_prices_each_token_class_by_model() {
        let mut t = trial("v", "t", true, 0, 0);
        t.model = Some("m".into());
        t.cost = Cost {
            input_tokens: 1000,
            cache_read_tokens: 600,
            cache_write_tokens: 100,
            output_tokens: 50,
            ..Cost::default()
        };
        // Default relative prices: 300 uncached + 60 read + 100 written + 200 output.
        assert!((weighted_cost(&t, &PriceTable::default()) - 660.0).abs() < 1e-9);
        let table = PriceTable::parse(
            r#"{"_comment":"x","default":{"input":1,"cache_read":0.1,"cache_write":1,"output":4},
                "m":{"input":2,"cache_read":0.5,"cache_write":2,"output":8}}"#,
        )
        .unwrap();
        assert!((weighted_cost(&t, &table) - (600.0 + 300.0 + 200.0 + 400.0)).abs() < 1e-9);
        assert!(PriceTable::parse(r#"{"m":{"input":1}}"#).is_err());
    }

    #[test]
    fn the_default_paired_checks_parse_and_cover_success_and_cost() {
        let checks: Vec<PairedCheck> =
            DEFAULT_PAIRED.split(',').map(|c| PairedCheck::parse(c).unwrap()).collect();
        let metrics: Vec<&str> = checks.iter().map(|c| c.metric.as_str()).collect();
        assert_eq!(metrics, ["success_rate", "cost_per_success"]);
        assert!(checks.iter().all(|c| c.not_significantly_worse));
    }

    #[test]
    fn the_paired_gate_needs_enough_pairs_and_reads_the_interval() {
        let run = |variant: &str, task: usize, success: bool, input: u64| Trial {
            cost: Cost { input_tokens: input, ..Cost::default() },
            ..trial(variant, &format!("t{task}"), success, 0, 0)
        };
        let mut trials = Vec::new();
        for task in 0..40 {
            // Both variants fail one task in forty, different ones; the candidate is 20% cheaper.
            trials.push(run("base", task, task != 3, 1000));
            trials.push(run("cand", task, task != 7, 800));
        }
        trials.push(run("cand", 99, true, 1)); // unpaired: ignored
        assert_eq!(pair_trials(&trials, "long-task", "base", "cand").len(), 40);
        let prices = PriceTable::default();
        let gate = Gate {
            paired: vec![
                PairedCheck::parse("success_rate:0.10").unwrap(),
                PairedCheck::parse("weighted_cost:0").unwrap(),
            ],
            ..Gate::default()
        };
        assert_eq!(gate.check_trials(&trials, &prices, "base", "cand"), vec![]);
        // Reversed, the cost check fails: +25% with no spread.
        let failures = gate.check_trials(&trials, &prices, "cand", "base");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].metric, "paired weighted_cost");
        // One discordant pair each way already spans ±7.5 points at forty pairs, so a five-point
        // tolerance cannot pass any run with noise; ten points can.
        let five = Gate { paired: vec![PairedCheck::parse("success_rate:0.05").unwrap()], ..Gate::default() };
        assert_eq!(five.check_trials(&trials, &prices, "base", "cand").len(), 1);
        // Three net losses do fail at ten points.
        let mut lost = trials.clone();
        for task in ["t10", "t11", "t12"] {
            lost.iter_mut().find(|t| t.variant == "cand" && t.task == task).unwrap().success = false;
        }
        let failures = gate.check_trials(&lost, &prices, "base", "cand");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].metric, "paired success_rate");
        let few = Gate { min_pairs: 50, ..gate.clone() };
        let failures = few.check_trials(&trials, &prices, "base", "cand");
        assert!(failures.iter().all(|f| f.detail.starts_with("insufficient samples: 40 pairs")), "{failures:?}");
        assert!(PairedCheck::parse("success_rate").is_err());
    }

    #[test]
    fn cost_per_success_judges_what_an_answer_costs() {
        let run = |variant: &str, task: usize, success: bool, input: u64| Trial {
            cost: Cost { input_tokens: input, ..Cost::default() },
            ..trial(variant, &format!("t{task}"), success, 0, 0)
        };
        let prices = PriceTable::default();
        let default_gate = || Gate {
            paired: DEFAULT_PAIRED.split(',').map(|c| PairedCheck::parse(c).unwrap()).collect(),
            improve: DEFAULT_IMPROVE.split(',').map(str::to_string).collect(),
            ..Gate::default()
        };
        // The candidate costs 20% more per task but answers 36 of 40 instead of 20: each correct
        // answer is cheaper. The old default (`weighted_cost:0`) fails it; DEC-13's passes it.
        let mut trials = Vec::new();
        for task in 0..40 {
            trials.push(run("base", task, task % 2 == 0, 1000));
            trials.push(run("cand", task, task % 10 != 0, 1200));
        }
        assert_eq!(default_gate().check_trials(&trials, &prices, "base", "cand"), vec![]);
        let old = Gate {
            paired: vec![PairedCheck::parse("weighted_cost:0").unwrap()],
            ..Gate::default()
        };
        assert_eq!(old.check_trials(&trials, &prices, "base", "cand").len(), 1);
        // Same answers at four times the price per task: significantly dearer per answer.
        let mut dear = Vec::new();
        for task in 0..40 {
            dear.push(run("base", task, task % 2 == 0, 1000));
            dear.push(run("cand", task, task % 2 == 0, 4000));
        }
        let failures = default_gate().check_trials(&dear, &prices, "base", "cand");
        let metrics: Vec<&str> = failures.iter().map(|f| f.metric.as_str()).collect();
        assert_eq!(metrics, ["improve success_rate|cost_per_success", "paired cost_per_success"], "{failures:?}");
        assert!(failures[1].detail.ends_with("significantly higher"), "{failures:?}");
        // The same price and the same answers: not worse, but not better either — it fails.
        let mut same = Vec::new();
        for task in 0..40 {
            same.push(run("base", task, task % 2 == 0, 1000));
            same.push(run("cand", task, task % 2 == 0, 1000));
        }
        let failures = default_gate().check_trials(&same, &prices, "base", "cand");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(failures[0].detail, "none of these is significantly better");
        let result = cost_per_success_result(&pair_trials(&dear, "long-task", "base", "cand"), &prices);
        assert!((result.mean - 3.0).abs() < 1e-9, "{result:?}");
    }

    #[test]
    fn the_bootstrap_is_deterministic_and_brackets_the_mean() {
        let diffs: Vec<f64> = (0..30).map(|i| f64::from(i % 5) - 2.0).collect();
        let (low, high) = bootstrap_interval(&diffs);
        assert_eq!(bootstrap_interval(&diffs), (low, high));
        assert!(low < 0.0 && high > 0.0 && low > -1.5 && high < 1.5, "{low} {high}");
        assert_eq!(bootstrap_interval(&[0.5; 10]), (0.5, 0.5));
    }
}
