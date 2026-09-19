//! 0.2.70 host-side Verifiable Runtime command surface.

#[path = "support/evidence_io.rs"]
mod evidence_io;

use std::path::PathBuf;
use std::process::ExitCode;

use deepstrike_core::runtime::verifiable::{
    EvidenceBundle, InspectReport, ReplayOptions, ReplayReport, VerifiableOperation, VerifyOptions,
    VerifyReport,
};
use evidence_io::{read_checkpoint, read_journal, read_session_log};

const USAGE: &str = "Usage: deepstrike <inspect|verify|replay|fork> <operation> \
                     [--journal <path>...] [--session-log <path>...] [--checkpoint <path>...] \
                     [--strict] [--require-complete] [--at <step>] [--output <path>] \
                     [--format human|json]\n\
                     \n\
                     Exit codes: 0 = pass, 1 = proven contradiction, 2 = insufficient evidence,\n\
                     64 = usage error";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Inspect,
    Verify,
    Replay,
    Fork,
}

struct Options {
    command: Command,
    operation_id: String,
    journals: Vec<PathBuf>,
    session_logs: Vec<PathBuf>,
    checkpoints: Vec<PathBuf>,
    strict: bool,
    require_complete: bool,
    at_step: Option<u64>,
    output: Option<PathBuf>,
    format: String,
}

fn main() -> ExitCode {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => return usage(&message),
    };
    let mut journals = Vec::new();
    for path in &options.journals {
        if let Err(message) = read_journal(path, &mut journals) {
            return evidence_error(&message);
        }
    }
    let mut session_logs = Vec::new();
    for path in &options.session_logs {
        if let Err(message) = read_session_log(path, &mut session_logs) {
            return evidence_error(&message);
        }
    }
    let mut checkpoints = Vec::new();
    for path in &options.checkpoints {
        if let Err(message) = read_checkpoint(path, &mut checkpoints) {
            return evidence_error(&message);
        }
    }

    let operation = VerifiableOperation::new(
        options.operation_id.clone(),
        EvidenceBundle::new(journals, session_logs, checkpoints),
    );

    match options.command {
        Command::Inspect => {
            let report = operation.inspect(options.strict);
            emit(&report, &options.format, print_inspect);
            ExitCode::from(report.validation.exit_code() as u8)
        }
        Command::Verify => {
            let report = operation.verify(VerifyOptions {
                strict: options.strict,
                require_complete: options.require_complete,
            });
            emit(&report, &options.format, print_verify);
            ExitCode::from(report.verdict.exit_code(options.require_complete) as u8)
        }
        Command::Replay => {
            let report = operation.replay(ReplayOptions {
                strict: options.strict,
                at_step: options.at_step,
            });
            emit(&report, &options.format, print_replay);
            ExitCode::from(match report.verdict {
                deepstrike_core::runtime::verifiable::ReplayVerdict::Pass => 0,
                deepstrike_core::runtime::verifiable::ReplayVerdict::Fail => 1,
                deepstrike_core::runtime::verifiable::ReplayVerdict::Unavailable => 2,
            })
        }
        Command::Fork => run_fork(&options, &operation),
    }
}

fn run_fork(options: &Options, operation: &VerifiableOperation) -> ExitCode {
    let Some(at_step) = options.at_step else {
        return usage("fork requires --at <step>");
    };
    let Some(output) = options.output.as_ref() else {
        return usage("fork requires --output <path>");
    };
    let replay = operation.replay(ReplayOptions {
        strict: options.strict,
        at_step: Some(at_step),
    });
    if replay.verdict != deepstrike_core::runtime::verifiable::ReplayVerdict::Pass {
        emit(&replay, &options.format, print_replay);
        return ExitCode::from(match replay.verdict {
            deepstrike_core::runtime::verifiable::ReplayVerdict::Fail => 1,
            _ => 2,
        });
    }
    let manifest = match operation.prepare_fork(at_step, options.strict) {
        Ok(plan) => plan.manifest(),
        Err(message) => {
            eprintln!("deepstrike: {message}");
            return ExitCode::from(2);
        }
    };
    let json = match serde_json::to_string_pretty(&manifest) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("deepstrike: could not serialize fork manifest: {error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = std::fs::write(output, &json) {
        eprintln!("deepstrike: could not write {}: {error}", output.display());
        return ExitCode::from(2);
    }
    if options.format == "json" {
        println!("{json}");
    } else {
        println!("fork manifest written to {}", output.display());
        println!(
            "{} step {} ← {}",
            manifest.operation_id, manifest.at_step, manifest.parent_record_digest
        );
    }
    ExitCode::from(0)
}

fn parse_options() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let command = match args.next().as_deref() {
        Some("inspect") => Command::Inspect,
        Some("verify") => Command::Verify,
        Some("replay") => Command::Replay,
        Some("fork") => Command::Fork,
        Some("--help") | Some("-h") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => return Err(format!("unknown command: {other}")),
        None => return Err("a command and operation are required".to_string()),
    };
    let operation_id = args
        .next()
        .ok_or_else(|| "an operation id is required".to_string())?;
    if operation_id.starts_with('-') {
        return Err("an operation id is required before options".to_string());
    }
    let mut options = Options {
        command,
        operation_id,
        journals: Vec::new(),
        session_logs: Vec::new(),
        checkpoints: Vec::new(),
        strict: false,
        require_complete: false,
        at_step: None,
        output: None,
        format: "human".to_string(),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--journal" => options.journals.push(next_path(&mut args, "--journal")?),
            "--session-log" => options
                .session_logs
                .push(next_path(&mut args, "--session-log")?),
            "--checkpoint" => options
                .checkpoints
                .push(next_path(&mut args, "--checkpoint")?),
            "--strict" => options.strict = true,
            "--require-complete" => options.require_complete = true,
            "--at" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--at requires a step".to_string())?;
                options.at_step = Some(
                    value
                        .parse()
                        .map_err(|_| "--at requires an integer".to_string())?,
                );
            }
            "--output" => options.output = Some(next_path(&mut args, "--output")?),
            "--format" => {
                let mode = args
                    .next()
                    .ok_or_else(|| "--format requires human or json".to_string())?;
                if mode != "human" && mode != "json" {
                    return Err("--format requires human or json".to_string());
                }
                options.format = mode;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if options.command != Command::Fork && options.output.is_some() {
        return Err("--output is only valid for fork".to_string());
    }
    if !matches!(options.command, Command::Replay | Command::Fork) && options.at_step.is_some() {
        return Err("--at is only valid for replay and fork".to_string());
    }
    if options.command != Command::Verify && options.require_complete {
        return Err("--require-complete is only valid for verify".to_string());
    }
    if options.journals.is_empty()
        && options.session_logs.is_empty()
        && options.checkpoints.is_empty()
    {
        return Err("at least one evidence source is required".to_string());
    }
    Ok(options)
}

fn next_path(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} requires a path"))
}

fn emit<T: serde::Serialize>(report: &T, format: &str, human: impl FnOnce(&T)) {
    if format == "json" {
        match serde_json::to_string_pretty(report) {
            Ok(json) => println!("{json}"),
            Err(error) => eprintln!("deepstrike: could not serialize the report: {error}"),
        }
    } else {
        human(report);
    }
}

fn print_inspect(report: &InspectReport) {
    println!(
        "operation {}: {} record(s)",
        report.operation_id,
        report.records.len()
    );
    for record in &report.records {
        println!(
            "  step {} {} input={} digest={}",
            record.step_seq, record.input_kind, record.input_id, record.record_digest
        );
    }
    println!("validator exit code: {}", report.validation.exit_code());
}

fn print_verify(report: &VerifyReport) {
    println!("operation {}: {:?}", report.operation_id, report.verdict);
    println!("validator exit code: {}", report.validation.exit_code());
}

fn print_replay(report: &ReplayReport) {
    println!(
        "operation {} replay: {:?} ({} step(s))",
        report.operation_id, report.verdict, report.compared_steps
    );
    if let Some(detail) = &report.first_divergence {
        println!("  {detail}");
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("deepstrike: {message}\n\n{USAGE}");
    ExitCode::from(64)
}

fn evidence_error(message: &str) -> ExitCode {
    eprintln!("deepstrike: {message}");
    ExitCode::from(2)
}
