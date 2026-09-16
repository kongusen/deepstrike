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
//!
//! SessionLog input forms (batch 3), per `--session-log` path:
//! - a directory: every `*.json`/`*.jsonl` file inside (name-sorted), each file one session
//!   stream — events keep the file's append order, and streams never cross-join;
//! - a file: one stream — a JSON object (one event), a JSON array (one event per element), or
//!   JSON Lines (one event per non-empty line).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use deepstrike_core::runtime::chain_validator::{
    ValidationReport, Verdict, validate_journal, validate_with_session_log,
};

const USAGE: &str = "Usage: ds-chain-validator --journal <path> [--journal <path>...] \
                     [--session-log <path>...] [--format human|json]\n\
                     \n\
                     Validates a kernel journal prefix against P2 §5 rules C1–C4, marking\n\
                     degraded evidence per C7. With --session-log, the batch-3 cross-checks\n\
                     (C6/C8) join the SessionLog evidence plane against the journal.\n\
                     Exit codes: 0 = all green, 1 = violation proven, 2 = evidence insufficient.\n\
                     \n\
                     --journal <path>     record source: directory of *.json records, a JSON\n\
                     \x20                    object/array file, or a JSONL file (repeatable)\n\
                     --session-log <path> session-log source: a directory (each *.json/*.jsonl\n\
                     \x20                    file is one append-ordered stream) or a single\n\
                     \x20                    stream file — object/array/JSONL (repeatable)\n\
                     --format <mode>      human (default) or json\n\
                     \n\
                     Batch 3 does not consume checkpoint inputs; the C5 checkpoint\n\
                     self-consistency rule and the launch-token ledger arrive with batch 2.";

fn main() -> ExitCode {
    let mut journals: Vec<PathBuf> = Vec::new();
    let mut session_logs: Vec<PathBuf> = Vec::new();
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
            "--session-log" => {
                let Some(path) = args.next() else {
                    return usage("--session-log requires a path");
                };
                session_logs.push(PathBuf::from(path));
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
            "--checkpoint" => {
                return usage(
                    "--checkpoint is consumed by batch-2 rules; batch-1/batch-3 rules do not read it",
                );
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

    let mut streams: Vec<Vec<Vec<u8>>> = Vec::new();
    for path in &session_logs {
        let streams_before = streams.len();
        match read_session_log(path, &mut streams) {
            Ok(count) => eprintln!(
                "read {count} session event(s) in {} stream(s) from {}",
                streams.len() - streams_before,
                path.display()
            ),
            Err(message) => {
                eprintln!("ds-chain-validator: {message}");
                return ExitCode::from(2);
            }
        }
    }

    let report = if session_logs.is_empty() {
        validate_journal(&blobs)
    } else {
        validate_with_session_log(&blobs, &streams)
    };
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
    split_blob_file(&bytes, blobs);
    Ok(blobs.len() - before)
}

/// Collect SessionLog event streams from one session-log source. Unlike the journal plane,
/// append order within one file is meaningful, so **each file becomes one stream** — for a
/// directory, every `*.json`/`*.jsonl` file inside (name-sorted); for a single path, the file
/// itself. Event blobs within a stream preserve the file's order.
fn read_session_log(path: &Path, streams: &mut Vec<Vec<Vec<u8>>>) -> Result<usize, String> {
    let mut added = 0usize;
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(|error| format!("cannot read directory {}: {error}", path.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|entry| {
                entry.extension().is_some_and(|extension| {
                    extension == "json" || extension == "jsonl"
                })
            })
            .collect();
        entries.sort();
        for entry in entries {
            let bytes = std::fs::read(&entry)
                .map_err(|error| format!("cannot read {}: {error}", entry.display()))?;
            let mut stream = Vec::new();
            split_blob_file(&bytes, &mut stream);
            added += stream.len();
            streams.push(stream);
        }
        return Ok(added);
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let mut stream = Vec::new();
    split_blob_file(&bytes, &mut stream);
    added += stream.len();
    streams.push(stream);
    Ok(added)
}

/// Split one file's bytes into blobs: a JSON array yields one blob per element, a single JSON
/// value yields one blob, anything else parses as JSON Lines. Array elements are re-emitted
/// compactly — safe for journal records because the record's self-digest is computed from its
/// decoded fields, not its input byte layout, and safe for SessionLog events because the
/// session plane is opaque JSON throughout.
fn split_blob_file(bytes: &[u8], blobs: &mut Vec<Vec<u8>>) {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Array(records)) => {
            for record in records {
                blobs.push(serde_json::to_vec(&record).unwrap_or_default());
            }
        }
        Ok(_) => blobs.push(bytes.to_vec()),
        Err(_) => {
            for line in bytes.split(|byte| *byte == b'\n') {
                let line = trim_ascii(line);
                if !line.is_empty() {
                    blobs.push(line.to_vec());
                }
            }
        }
    }
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
    if let Some(events) = report.session_events {
        println!("session-log plane: {events} parseable event(s)");
    }
    if report.unparseable_events > 0 {
        println!(
            "unparseable session event blob(s): {} (evidence insufficient, not a violation)",
            report.unparseable_events
        );
    }
    for rule in &report.cross_checks {
        let label = match rule.verdict {
            Verdict::Pass => "pass",
            Verdict::Fail => "FAIL",
            Verdict::Degraded => "degraded",
        };
        println!("cross-check {} {label}: {}", rule.rule, rule.detail);
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
