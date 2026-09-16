//! `ds-chain-validator` — the P7-S5 chain validator CLI (P2 §5 rules C1–C4 + C7, batch 1).
//!
//! Host-ops tooling: CI gates and incident triage run the same knife. Input is a journal prefix
//! — a sequence of kernel record blobs — grouped into per-operation chain segments, each judged
//! independently. Record bytes are passed through untouched: a verdict is about the bytes the
//! host durably wrote, never about a re-serialization.
//!
//! Exit codes (P7 §3.2): `0` every check green (degraded hops and deferred scope do not count
//! against green), `1` a violation was proven, `2` the evidence was insufficient (nothing
//! judgeable, or unparseable blobs and no proven violation). `64` is a usage error.
//!
//! Journal input forms, per `--journal` path:
//! - a directory: every `*.json` file inside (name-sorted), each file one record;
//! - a file holding one JSON object: one record;
//! - a file holding a JSON array: its elements, one record each;
//! - anything else: JSON Lines — one record per non-empty line.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use deepstrike_core::runtime::chain_validator::{ValidationReport, Verdict, validate_journal};

const USAGE: &str = "Usage: ds-chain-validator --journal <path> [--journal <path>...] \
                     [--format human|json]\n\
                     \n\
                     Validates a kernel journal prefix against P2 §5 rules C1–C4 (batch 1), \
                     marking degraded evidence per C7.\n\
                     Exit codes: 0 = all green, 1 = violation proven, 2 = evidence insufficient.\n\
                     \n\
                     --journal <path>   record source: directory of *.json records, a JSON\n\
                     \x20                   object/array file, or a JSONL file (repeatable)\n\
                     --format <mode>    human (default) or json\n\
                     \n\
                     Batch 1 does not consume checkpoint or SessionLog inputs; the C5\n\
                     checkpoint self-consistency rule and the launch-token ledger arrive with\n\
                     batch 2.";

fn main() -> ExitCode {
    let mut journals: Vec<PathBuf> = Vec::new();
    let mut format = "human".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--journal" => {
                let Some(path) = args.next() else {
                    return usage("--journal requires a path");
                };
                journals.push(PathBuf::from(path));
            }
            "--format" => {
                let Some(mode) = args.next() else {
                    return usage("--format requires human or json");
                };
                if mode != "human" && mode != "json" {
                    return usage("--format requires human or json");
                }
                format = mode;
            }
            "--checkpoint" | "--session-log" => {
                return usage(&format!(
                    "{arg} is consumed by batch-2 rules; batch-1 rules C1–C4+C7 do not read it"
                ));
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::from(0);
            }
            _ => return usage(&format!("unknown argument: {arg}")),
        }
    }
    if journals.is_empty() {
        return usage("at least one --journal path is required");
    }

    let mut blobs: Vec<Vec<u8>> = Vec::new();
    for path in &journals {
        match read_journal(path, &mut blobs) {
            Ok(count) => eprintln!("read {count} record blob(s) from {}", path.display()),
            Err(message) => {
                eprintln!("ds-chain-validator: {message}");
                return ExitCode::from(2);
            }
        }
    }

    let report = validate_journal(&blobs);
    match format.as_str() {
        "json" => match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("ds-chain-validator: could not serialize the report: {error}");
                return ExitCode::from(2);
            }
        },
        _ => print_human(&report),
    }

    let code = report.exit_code();
    ExitCode::from(u8::try_from(code).unwrap_or(2))
}

fn usage(message: &str) -> ExitCode {
    eprintln!("ds-chain-validator: {message}\n\n{USAGE}");
    ExitCode::from(64)
}

/// Collect record blobs from one journal source. Byte-exactness is the whole game: directory
/// entries and whole-file objects pass through raw; array elements and JSONL lines are handed
/// over exactly as found (array elements are re-emitted compactly — safe because the record's
/// self-digest is computed from its decoded fields, not its input byte layout).
fn read_journal(path: &Path, blobs: &mut Vec<Vec<u8>>) -> Result<usize, String> {
    let before = blobs.len();
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|error| format!("cannot read directory {}: {error}", path.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|entry| {
                entry
                    .extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        entries.sort();
        for entry in entries {
            let bytes = std::fs::read(&entry)
                .map_err(|error| format!("cannot read {}: {error}", entry.display()))?;
            blobs.push(bytes);
        }
        return Ok(blobs.len() - before);
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Array(records)) => {
            for record in records {
                blobs.push(serde_json::to_vec(&record).unwrap_or_default());
            }
        }
        Ok(serde_json::Value::Object(_)) => blobs.push(bytes),
        Ok(_) => blobs.push(bytes), // a scalar/array-of-scalars: the validator marks it unparseable
        Err(_) => {
            for line in bytes.split(|byte| *byte == b'\n') {
                let line = trim_ascii(line);
                if !line.is_empty() {
                    blobs.push(line.to_vec());
                }
            }
        }
    }
    Ok(blobs.len() - before)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |position| position + 1);
    &bytes[start..end]
}

fn print_human(report: &ValidationReport) {
    for segment in &report.segments {
        println!("segment {} ({} hop(s))", segment.operation_id, segment.hops);
        for rule in &segment.rules {
            let label = match rule.verdict {
                Verdict::Pass => "pass",
                Verdict::Fail => "FAIL",
                Verdict::Degraded => "degraded",
            };
            println!("  {} {label}: {}", rule.rule, rule.detail);
        }
        for hop in &segment.degraded_hops {
            println!(
                "  degraded hop #{} (step {}): {}",
                hop.ordinal,
                hop.step_seq.map_or("?".to_string(), |seq| seq.to_string()),
                hop.reason,
            );
        }
    }
    if report.unparseable_records > 0 {
        println!(
            "unparseable record blob(s): {} (evidence insufficient, not a violation)",
            report.unparseable_records
        );
    }
    for deferred in &report.deferred {
        println!("deferred: {deferred}");
    }
    let segments = report.segments.len();
    let verdict = match report.exit_code() {
        0 => "all green",
        1 => "VIOLATION",
        _ => "evidence insufficient",
    };
    println!(
        "summary: {segments} segment(s), exit {} ({verdict})",
        report.exit_code()
    );
}
