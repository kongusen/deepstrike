//! Plan task 9.4 · offline consolidation is an ordinary run, not a kernel mechanism.
//!
//! Between sessions, a host may ask a model to reconcile its long-term memory against what
//! actually happened. That is just an agent run: the model reads memory with `memory`, searches the
//! session transcripts with `transcript_search`, and proposes corrected or missing records with
//! `propose_memory`, which the kernel admits like any other proposal. When it runs and what it
//! costs is the host's decision; the kernel does not know consolidation exists.
//! Mirrors `node/src/memory/consolidate.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::runtime::{LocalExecutionPlane, RuntimeOptions, RuntimeRunner, SessionEntry, SessionLog};
use crate::tools::RegisteredTool;

pub const TRANSCRIPT_SEARCH_TOOL_NAME: &str = "transcript_search";
const SNIPPET_CHARS: usize = 240;

pub const DEFAULT_CONSOLIDATION_GOAL: &str = "Reconcile long-term memory with what actually \
happened in the earlier sessions. Read the memory records that matter (memory), and check each \
against the transcripts (transcript_search). Where the transcripts contradict a record, propose a \
corrected record with the same name; where they show a rule or lesson memory lacks, propose it; \
finish by proposing one index record that says which records cover what. Cite the session and turn \
a record rests on in its content. Reply with a short list of what you proposed.";

/// One searchable line of a session transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLine {
    pub session_id: String,
    pub seq: u64,
    pub turn: u64,
    /// Where the text came from: the goal, the model's text, or a tool result (with its call).
    pub source: String,
    pub text: String,
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The searchable lines of one session's events: the goal, the model's text and its calls, and
/// every tool result labeled with the call that produced it.
pub fn transcript_lines(session_id: &str, entries: &[SessionEntry]) -> Vec<TranscriptLine> {
    let mut calls: HashMap<String, String> = HashMap::new();
    let mut lines = Vec::new();
    for entry in entries {
        let event = serde_json::to_value(&entry.event).unwrap_or(Value::Null);
        let turn = event["turn"].as_u64().unwrap_or(0);
        let line = |source: String, body: String| TranscriptLine {
            session_id: session_id.to_string(),
            seq: entry.seq,
            turn,
            source,
            text: body,
        };
        match event["kind"].as_str() {
            Some("run_started") => lines.push(line("goal".into(), text(&event["goal"]))),
            Some("llm_completed") => {
                for call in event["tool_calls"].as_array().into_iter().flatten() {
                    calls.insert(
                        text(&call["id"]),
                        format!("{} {}", text(&call["name"]), text(&call["arguments"])),
                    );
                }
                let said = text(&event["content"]);
                if !said.trim().is_empty() {
                    lines.push(line("model".into(), said));
                }
            }
            Some("tool_completed") => {
                for result in event["results"].as_array().into_iter().flatten() {
                    let call_id = text(&result["call_id"]);
                    let source = format!(
                        "{call_id} {}",
                        calls.get(&call_id).map(String::as_str).unwrap_or("tool")
                    );
                    lines.push(line(source, text(&result["output"])));
                }
            }
            _ => {}
        }
    }
    lines
}

fn terms(query: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for term in query
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
    {
        if term.chars().count() > 1 && !seen.iter().any(|s: &String| s == term) {
            seen.push(term.to_string());
        }
    }
    seen
}

/// Lines ranked by how many query terms they contain (ties: earlier first), each cut to a snippet
/// around its first hit. Deterministic.
pub fn search_transcripts(lines: &[TranscriptLine], query: &str, limit: usize) -> Vec<TranscriptLine> {
    let wanted = terms(query);
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, usize, usize)> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let folded = line.text.to_lowercase();
            let source = line.source.to_lowercase();
            let hits: Vec<&String> =
                wanted.iter().filter(|t| folded.contains(t.as_str()) || source.contains(t.as_str())).collect();
            (!hits.is_empty()).then(|| {
                let first = folded.find(hits[0].as_str()).unwrap_or(0);
                (hits.len(), index, first)
            })
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(limit.max(1))
        .map(|(_, index, first)| {
            let line = &lines[index];
            // Byte offset of the hit in the lowercased text, mapped to a char offset of the
            // original (lowercasing can change byte lengths).
            let hit_char = line.text.to_lowercase()[..first].chars().count();
            let start = hit_char.saturating_sub(SNIPPET_CHARS / 2);
            let total = line.text.chars().count();
            let snippet: String = line.text.chars().skip(start).take(SNIPPET_CHARS).collect();
            let prefix = if start > 0 { "…" } else { "" };
            let suffix = if start + SNIPPET_CHARS < total { "…" } else { "" };
            TranscriptLine { text: format!("{prefix}{snippet}{suffix}"), ..line.clone() }
        })
        .collect()
}

/// A host tool that searches session transcripts by keyword. Each result names its session, turn
/// and source (the goal, the model, or `call_id tool arguments`), so a model can cite it.
pub fn transcript_search_tool(
    session_log: Arc<dyn SessionLog>,
    session_ids: Vec<String>,
    max_results: usize,
) -> RegisteredTool {
    let cap = max_results.max(1);
    RegisteredTool::text(
        TRANSCRIPT_SEARCH_TOOL_NAME,
        "Search the transcripts of earlier sessions: what was asked, what the model said, and what \
         each tool returned. Use it to check a memory against what actually happened.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Words to look for." },
                "session_id": { "type": "string", "description": "Search only this session." },
                "limit": { "type": "integer", "description": format!("Results to return (at most {cap}).") }
            },
            "required": ["query"]
        }),
        move |args: Value| {
            let log = session_log.clone();
            let ids = session_ids.clone();
            Box::pin(async move {
                let only: Vec<String> = match args["session_id"].as_str().filter(|s| !s.is_empty()) {
                    Some(one) => vec![one.to_string()],
                    None => ids.clone(),
                };
                let mut lines = Vec::new();
                for session in only.iter().filter(|s| ids.contains(s)) {
                    let entries = log.read(session, 0, None).await?;
                    lines.extend(transcript_lines(session, &entries));
                }
                let limit = args["limit"].as_u64().map_or(cap, |n| (n as usize).clamp(1, cap));
                let found = search_transcripts(&lines, args["query"].as_str().unwrap_or(""), limit);
                if found.is_empty() {
                    return Ok("no matching lines".to_string());
                }
                Ok(found
                    .iter()
                    .map(|l| format!("[{} · turn {} · {}] {}", l.session_id, l.turn, l.source, l.text))
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        },
    )
}

/// Run one consolidation: an ordinary agent run with `transcript_search` added to its tools. The
/// host's memory options in `options` (store, candidate sink, runtime policy, agent id, scope)
/// are kept as they are; proposals go through the candidate ABI as usual. `options.session_log`
/// is required: the transcripts are read from it and the run is recorded there.
pub async fn consolidate_memory(
    mut options: RuntimeOptions,
    session_ids: Vec<String>,
    goal: Option<&str>,
    max_results: usize,
) -> crate::Result<(String, String)> {
    let log = options
        .session_log
        .clone()
        .ok_or_else(|| crate::Error::Other("consolidate_memory needs RuntimeOptions.session_log".into()))?;
    let mut plane = LocalExecutionPlane::new();
    plane.register(transcript_search_tool(log, session_ids.clone(), max_results));
    options.execution_plane = Some(Box::new(plane));
    options.baseline_tool_ids = Some(vec![TRANSCRIPT_SEARCH_TOOL_NAME.into()]);
    let session_id = options
        .session_id
        .clone()
        .unwrap_or_else(|| format!("consolidate-{}", session_ids.len()));
    options.session_id = Some(session_id.clone());
    let runner = RuntimeRunner::new(options);
    let stream = runner
        .run_streaming(goal.unwrap_or(DEFAULT_CONSOLIDATION_GOAL), &[], None, Some(&session_id))
        .await?;
    let text = crate::runtime::collect_text(stream).await?;
    Ok((session_id, text))
}
