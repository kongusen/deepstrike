//! `deepstrike-lab` · compare context-policy variants from recorded trials (plan task 4.3).
//!
//!   deepstrike-lab report <trials.jsonl>... [--baseline <variant>] [--json]
//!   deepstrike-lab gate <trials.jsonl>... --baseline <variant> --candidate <variant>
//!                       [--no-worse <metric,...>] [--max-estimate-error <fraction>]
//!
//! `report` prints a Markdown table per scenario (or the summaries as JSON). `gate` prints each
//! failed check and exits 1 if there is one.

use std::process::ExitCode;

use deepstrike_lab::{Gate, markdown, parse_trials, summarize};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("deepstrike-lab: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let (command, rest) = args.split_first().ok_or_else(usage)?;
    let mut files = Vec::new();
    let mut flags = std::collections::BTreeMap::<String, String>::new();
    let mut json = false;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => json = true,
            flag if flag.starts_with("--") => {
                let value = rest.next().ok_or_else(|| format!("{flag} needs a value"))?;
                flags.insert(flag.trim_start_matches("--").to_string(), value.clone());
            }
            file => files.push(file.to_string()),
        }
    }
    if files.is_empty() {
        return Err(usage());
    }
    let mut trials = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|error| format!("{file}: {error}"))?;
        trials.extend(parse_trials(&text).map_err(|error| format!("{file}: {error}"))?);
    }
    let summaries = summarize(&trials);
    match command.as_str() {
        "report" if json => {
            println!("{}", serde_json::to_string_pretty(&summaries).map_err(|e| e.to_string())?);
            Ok(ExitCode::SUCCESS)
        }
        "report" => {
            print!("{}", markdown(&summaries, flags.get("baseline").map(String::as_str)));
            Ok(ExitCode::SUCCESS)
        }
        "gate" => {
            let baseline = flags.get("baseline").ok_or("gate needs --baseline")?;
            let candidate = flags.get("candidate").ok_or("gate needs --candidate")?;
            let gate = Gate {
                no_worse: flags
                    .get("no-worse")
                    .map(|list| list.split(',').map(|m| m.trim().to_string()).collect())
                    .unwrap_or_default(),
                max_estimate_error: flags
                    .get("max-estimate-error")
                    .map(|bound| bound.parse::<f64>().map_err(|e| format!("--max-estimate-error: {e}")))
                    .transpose()?,
            };
            let failures = gate.check(&summaries, baseline, candidate);
            for failure in &failures {
                println!("FAIL {} {}: {}", failure.scenario, failure.metric, failure.detail);
            }
            if failures.is_empty() {
                println!("PASS {candidate} is no worse than {baseline}");
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
        other => Err(format!("unknown command {other:?}\n{}", usage())),
    }
}

fn usage() -> String {
    "usage: deepstrike-lab report <trials.jsonl>... [--baseline <variant>] [--json]\n       \
     deepstrike-lab gate <trials.jsonl>... --baseline <v> --candidate <v> \
     [--no-worse m1,m2] [--max-estimate-error 0.10]"
        .to_string()
}
