//! Plan task 9.1 · notes the model writes, and the kernel keeps.
//!
//! The model decides what is worth keeping (a running count, a list of what is done, a rule it
//! learned) and writes it with the `note` tool. The kernel keeps the mechanics: notes live in the
//! working partition, so compaction never touches them; they render in the state turn; their total
//! is capped, and over the cap the kernel refuses the write rather than deleting for the model; and
//! every note is an item of the graph with `derived_from` edges to the evidence it came from,
//! `links_to` edges to the notes it names and `supersedes` edges to the notes it replaced.

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::item::{ItemId, Relation};
use super::manager::ContextManager;
use super::task_state::{Note, NoteKind};
use crate::types::message::{Content, ContentPart, Role, ToolSchema};

pub const NOTE_TOOL_NAME: &str = "note";

/// Longest title a note may have, in characters.
pub const NOTE_TITLE_MAX_CHARS: usize = 120;

/// Tool results a note written without explicit evidence derives from, at most.
pub const NOTE_DEFAULT_EVIDENCE_CAP: usize = 16;

/// Plan task 9.2 · results a page-out notice lists before `(+N more)`.
pub const PAGE_OUT_NOTICE_LINES: usize = 8;

/// Plan task 9.2 · characters of a call's arguments a notice line shows.
pub const PAGE_OUT_ARGS_CHARS: usize = 60;

/// What the model asks of the `note` tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteRequest {
    #[serde(default)]
    pub action: NoteAction,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NoteKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    /// On `retire`: the note that replaces this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_by: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteAction {
    #[default]
    Write,
    Retire,
}

impl ContextManager {
    pub fn notes_enabled(&self) -> bool {
        self.config.notes_budget_tokens > 0
    }

    pub fn note_tool_schema(&self) -> Option<ToolSchema> {
        if !self.notes_enabled() {
            return None;
        }
        Some(ToolSchema {
            name: CompactString::new(NOTE_TOOL_NAME),
            description: "Keep notes that survive compaction. As the conversation grows, older \
tool results and turns are archived; your notes stay in [TASK STATE] word for word. Write down \
what you work out and could not simply read again: running counts and totals, lists of what is \
done and what is left, conclusions, rules you learned. When it changes, write the same title \
again to update it. `evidence` names what it came from (call ids, file paths or arguments); \
without it the note is linked to the results you received since your last note. Retire a note \
you no longer need."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["write", "retire"], "description": "Default: write." },
                    "title": { "type": "string", "description": "The note's name; writing an existing title updates it." },
                    "kind": { "type": "string", "enum": ["state", "rule", "question", "index"], "description": "state: progress of this task (counts, done-lists); rule: a lesson or rule; question: something still open; index: where things are." },
                    "body": { "type": "string" },
                    "evidence": { "type": "array", "items": { "type": "string" } },
                    "links": { "type": "array", "items": { "type": "string" }, "description": "Titles of related notes." },
                    "replaced_by": { "type": "string", "description": "On retire: the note that replaces this one." }
                },
                "required": ["title"]
            }),
        })
    }

    /// Tokens the notes block takes in the state turn.
    pub fn notes_tokens(&self) -> u32 {
        let block = self.partitions.task_state.render_notes();
        if block.is_empty() { 0 } else { self.engine.count(&block) }
    }

    /// Apply one `note` call. `Ok` is the model-facing acknowledgement; `Err` the rejection.
    pub fn write_note(&mut self, request: &NoteRequest, turn: u32) -> Result<String, String> {
        if !self.notes_enabled() {
            return Err("notes are not enabled for this run".into());
        }
        let title = request.title.trim();
        if title.is_empty() {
            return Err("a note needs a title".into());
        }
        if title.chars().count() > NOTE_TITLE_MAX_CHARS {
            return Err(format!("a note title may have at most {NOTE_TITLE_MAX_CHARS} characters"));
        }
        match request.action {
            NoteAction::Retire => self.retire_note(title, request.replaced_by.as_deref()),
            NoteAction::Write => self.save_note(title, request, turn),
        }
    }

    fn retire_note(&mut self, title: &str, replaced_by: Option<&str>) -> Result<String, String> {
        let state = &self.partitions.task_state;
        let Some(index) = state.note_index(title) else {
            return Err(format!("there is no note titled {title:?}; {}", self.note_titles()));
        };
        let replacement = replaced_by.and_then(|other| state.note_index(other)).map(|i| state.notes[i].item);
        let old = self.partitions.task_state.notes.remove(index);
        self.partitions.task_state.notes_retired += 1;
        if let Some(new) = replacement {
            self.add_note_edge(new, old.item, Relation::Supersedes);
        }
        Ok(format!("note {:?} retired; {} note(s) left", old.title, self.partitions.task_state.notes.len()))
    }

    fn save_note(&mut self, title: &str, request: &NoteRequest, turn: u32) -> Result<String, String> {
        let body = request.body.as_deref().map(str::trim).unwrap_or("");
        if body.is_empty() {
            return Err("a note needs a body".into());
        }
        let existing = self.partitions.task_state.note_index(title);
        let kind = request
            .kind
            .or_else(|| existing.map(|i| self.partitions.task_state.notes[i].kind))
            .unwrap_or_default();
        let mut note = Note {
            title: title.to_string(),
            kind,
            body: body.to_string(),
            evidence: request.evidence.iter().map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect(),
            links: request.links.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
            cites: Vec::new(),
            archived: Vec::new(),
            item: existing.map_or(0, |i| self.partitions.task_state.notes[i].item),
            turn,
        };

        // The cap is checked on the notes as they would be: the kernel never deletes for the model.
        let budget = self.config.notes_budget_tokens;
        let previous = existing.map(|i| self.partitions.task_state.notes[i].clone());
        match existing {
            Some(i) => self.partitions.task_state.notes[i] = note.clone(),
            None => self.partitions.task_state.notes.push(note.clone()),
        }
        let used = self.notes_tokens();
        if used > budget {
            match (existing, previous) {
                (Some(i), Some(previous)) => self.partitions.task_state.notes[i] = previous,
                _ => {
                    self.partitions.task_state.notes.pop();
                }
            }
            return Err(format!(
                "notes are full ({used} of {budget} tokens with this one): merge notes into \
                 fewer, shorten them, or retire ones you no longer need, then write again"
            ));
        }

        if note.item == 0 {
            note.item = self.alloc_item_id();
            let index = existing.unwrap_or(self.partitions.task_state.notes.len() - 1);
            self.partitions.task_state.notes[index].item = note.item;
        }
        let evidence = self.note_evidence(&note.evidence);
        for (to, _) in &evidence {
            self.add_note_edge(note.item, *to, Relation::DerivedFrom);
        }
        let index = self.partitions.task_state.note_index(&note.title).expect("the note was just stored");
        self.partitions.task_state.notes[index].cites = evidence.iter().map(|(_, call)| call.clone()).collect();
        let mut dead = Vec::new();
        for link in &note.links {
            match self.partitions.task_state.note_index(link) {
                Some(i) => {
                    let to = self.partitions.task_state.notes[i].item;
                    self.add_note_edge(note.item, to, Relation::LinksTo);
                }
                None => dead.push(link.clone()),
            }
        }
        let state = &mut self.partitions.task_state;
        state.notes_written += 1;
        state.notes_watermark = self.last_item_id;
        let verb = if existing.is_some() { "updated" } else { "saved" };
        let mut ack = format!(
            "note {:?} {verb}; it shows in [TASK STATE] from your next turn ({} note(s), {used} of \
             {budget} tokens); linked to {} result(s)",
            note.title,
            self.partitions.task_state.notes.len(),
            evidence.len()
        );
        if !dead.is_empty() {
            ack.push_str(&format!("; no note yet for link(s) {dead:?}"));
        }
        Ok(ack)
    }

    /// Plan task 9.2 · record, on each note, which of its cited results `archived` just took out.
    pub(crate) fn mark_archived_citations(&mut self, archived: &[crate::types::message::CoreMessage]) {
        let gone: std::collections::BTreeSet<&str> = archived
            .iter()
            .filter_map(|message| match &message.content {
                Content::Parts(parts) => Some(parts.iter().filter_map(|part| match part {
                    ContentPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
                    _ => None,
                })),
                _ => None,
            })
            .flatten()
            .collect();
        for note in &mut self.partitions.task_state.notes {
            for call in &note.cites {
                if gone.contains(call.as_str()) && !note.archived.contains(call) {
                    note.archived.push(call.clone());
                }
            }
        }
    }

    /// Plan task 9.2 · whether a range compaction toward `target_tokens` should wait one turn so
    /// the model can note what it is about to lose. It waits only when the page-out notice is on,
    /// has not been given for this compaction yet, and the archive would take results no note
    /// cites; the notice lists them. A second call (the next turn) always lets it run.
    pub fn defer_for_page_out_notice(&mut self, target_tokens: u32) -> bool {
        if !self.config.page_out_notice || !self.partitions.task_state.page_out_notice.is_empty() {
            return false;
        }
        let partitions = &self.partitions;
        let non_history = partitions.total_tokens(&self.engine).saturating_sub(partitions.history.token_count());
        let cited = super::compression::cited_calls(partitions);
        let plan = super::utility::plan_utility_archive_with_measurements(
            partitions.history.messages(),
            partitions.history.measurements(),
            partitions.history.token_count(),
            target_tokens.saturating_sub(non_history),
            self.config.preserve_recent_turns,
            &self.engine,
            &super::utility::UtilitySelectionContext {
                goal: &partitions.task_state.goal,
                criteria: &partitions.task_state.criteria,
                preserved_refs: &partitions.task_state.preserved_refs,
                active_directives: &partitions.task_state.directives,
                cited_calls: &if self.config.compaction_reads_notes { cited.clone() } else { Default::default() },
            },
        );
        let messages = partitions.history.messages();
        let mut lines = Vec::new();
        for range in &plan.archived_ranges {
            for message in &messages[range.clone()] {
                let Content::Parts(parts) = &message.content else { continue };
                for part in parts {
                    let ContentPart::ToolResult { call_id, .. } = part else { continue };
                    if cited.contains(call_id.as_str()) {
                        continue;
                    }
                    let call = messages
                        .iter()
                        .flat_map(|m| m.tool_calls.iter())
                        .find(|call| call.id == *call_id);
                    let line = match call {
                        Some(call) => {
                            let mut args = call.arguments.to_string();
                            if args.chars().count() > PAGE_OUT_ARGS_CHARS {
                                args = args.chars().take(PAGE_OUT_ARGS_CHARS).collect::<String>() + "…";
                            }
                            format!("{call_id} {}({args})", call.name)
                        }
                        None => call_id.to_string(),
                    };
                    lines.push(line);
                }
            }
        }
        if lines.is_empty() {
            return false;
        }
        if lines.len() > PAGE_OUT_NOTICE_LINES {
            let more = lines.len() - PAGE_OUT_NOTICE_LINES;
            lines.truncate(PAGE_OUT_NOTICE_LINES);
            lines.push(format!("(+{more} more)"));
        }
        self.partitions.task_state.page_out_notice = lines;
        true
    }

    fn add_note_edge(&mut self, from: ItemId, to: ItemId, relation: Relation) {
        if from != 0 && to != 0 && from != to {
            self.add_graph_edge(from, to, relation);
        }
    }

    fn note_titles(&self) -> String {
        let titles: Vec<&str> = self.partitions.task_state.notes.iter().map(|n| n.title.as_str()).collect();
        if titles.is_empty() { "there are no notes".into() } else { format!("notes: {titles:?}") }
    }

    /// The items a note's evidence names. Each reference is a tool call id, a handle's source, or
    /// text found in a tool call's arguments (a path, a query); without references, the tool
    /// results that arrived since the last note was written.
    pub(crate) fn note_evidence(&self, references: &[String]) -> Vec<(ItemId, String)> {
        let history = &self.partitions.history;
        // (result item, call id) of every tool result still in history.
        let results: Vec<(ItemId, &str)> = (0..history.len())
            .filter(|&i| history.messages()[i].role == Role::Tool)
            .flat_map(|i| {
                let id = history.tag(i).id;
                match &history.messages()[i].content {
                    Content::Parts(parts) => parts
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::ToolResult { call_id, .. } => Some((id, call_id.as_str())),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                }
            })
            .filter(|(id, _)| *id != 0)
            .collect();
        let mut items: Vec<(ItemId, String)> = Vec::new();
        let push = |item: ItemId, call: &str, items: &mut Vec<(ItemId, String)>| {
            if item != 0 && !items.iter().any(|(id, _)| *id == item) {
                items.push((item, call.to_string()));
            }
        };
        if references.is_empty() {
            let watermark = self.partitions.task_state.notes_watermark;
            let fresh: Vec<&(ItemId, &str)> = results.iter().filter(|(id, _)| *id > watermark).collect();
            let skip = fresh.len().saturating_sub(NOTE_DEFAULT_EVIDENCE_CAP);
            for (id, call) in fresh.into_iter().skip(skip) {
                push(*id, call, &mut items);
            }
            return items;
        }
        // The item that holds call `call_id`'s result: in history, or archived behind a handle.
        let result_of = |call_id: &str| -> Option<ItemId> {
            results
                .iter()
                .find(|(_, id)| *id == call_id)
                .map(|(item, _)| *item)
                .or_else(|| {
                    self.handles
                        .all()
                        .iter()
                        .find(|h| h.source.as_deref() == Some(call_id))
                        .map(|h| h.item)
                })
        };
        for reference in references {
            if let Some(item) = result_of(reference) {
                push(item, reference, &mut items);
                continue;
            }
            if reference.chars().count() < 3 {
                continue;
            }
            for index in 0..history.len() {
                for call in &history.messages()[index].tool_calls {
                    if call.arguments.to_string().contains(reference.as_str())
                        && let Some(item) = result_of(call.id.as_str())
                    {
                        push(item, call.id.as_str(), &mut items);
                    }
                }
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::context::config::ContextConfig;
    use crate::context::pressure::PressureAction;
    use crate::types::message::{CoreMessage, ToolCall};

    fn manager(budget: u32) -> ContextManager {
        let mut mgr = ContextManager::with_config(
            100_000,
            ContextConfig { notes_budget_tokens: budget, ..ContextConfig::default() },
            ContextTokenEngine::char_approx(),
        );
        mgr.init_task("count the errors".into(), vec![]);
        mgr
    }

    fn read(mgr: &mut ContextManager, call_id: &str, path: &str, body: &str) {
        let mut call = CoreMessage::assistant("");
        call.tool_calls = vec![ToolCall {
            id: call_id.into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": path }),
        }];
        mgr.push_history(call, 5);
        mgr.push_history(
            CoreMessage::tool(vec![ContentPart::ToolResult {
                call_id: call_id.into(),
                output: body.into(),
                is_error: false,
                durable_content: None,
            }]),
            20,
        );
    }

    fn write(title: &str, body: &str) -> NoteRequest {
        NoteRequest { title: title.into(), body: Some(body.into()), ..NoteRequest::default() }
    }

    fn edges(mgr: &ContextManager, relation: Relation) -> Vec<(ItemId, ItemId)> {
        mgr.graph.edges().iter().filter(|e| e.relation == relation).map(|e| (e.from, e.to)).collect()
    }

    #[test]
    fn the_tool_is_offered_only_with_a_budget() {
        assert!(manager(0).note_tool_schema().is_none());
        assert!(manager(0).write_note(&write("t", "b"), 1).is_err());
        let mgr = manager(500);
        assert_eq!(mgr.note_tool_schema().unwrap().name, NOTE_TOOL_NAME);
        assert!(mgr.meta_tool_schemas().iter().any(|t| t.name == NOTE_TOOL_NAME));
    }

    #[test]
    fn a_note_renders_in_the_task_state_and_rewrites_by_title() {
        let mut mgr = manager(500);
        mgr.write_note(&write("Tally", "3 so far"), 1).unwrap();
        let ack = mgr.write_note(&NoteRequest { kind: Some(NoteKind::State), ..write(" tally ", "7 so far") }, 2).unwrap();
        assert!(ack.contains("updated"), "{ack}");
        let state = &mgr.partitions.task_state;
        assert_eq!(state.notes.len(), 1);
        assert_eq!(state.notes[0].body, "7 so far");
        assert_eq!(state.notes_written, 2);
        assert!(state.format_compact().contains("  - [state] tally: 7 so far"));
    }

    #[test]
    fn without_evidence_a_note_derives_from_the_results_since_the_last_note() {
        let mut mgr = manager(500);
        read(&mut mgr, "c1", "logs/a.log", "ERROR x");
        mgr.write_note(&write("A", "1"), 1).unwrap();
        read(&mut mgr, "c2", "logs/b.log", "ERROR y");
        read(&mut mgr, "c3", "logs/c.log", "ok");
        mgr.write_note(&write("B", "2"), 2).unwrap();
        let notes = &mgr.partitions.task_state.notes;
        let derived = edges(&mgr, Relation::DerivedFrom);
        assert_eq!(derived.iter().filter(|(from, _)| *from == notes[0].item).count(), 1);
        assert_eq!(derived.iter().filter(|(from, _)| *from == notes[1].item).count(), 2);
    }

    #[test]
    fn evidence_resolves_call_ids_and_arguments() {
        let mut mgr = manager(500);
        read(&mut mgr, "c1", "logs/a.log", "ERROR x");
        read(&mut mgr, "c2", "logs/b.log", "ERROR y");
        let request = NoteRequest { evidence: vec!["c1".into(), "logs/b.log".into(), "nowhere".into()], ..write("A", "2") };
        let ack = mgr.write_note(&request, 1).unwrap();
        assert!(ack.contains("linked to 2 result(s)"), "{ack}");
    }

    #[test]
    fn over_the_budget_the_write_is_refused_and_nothing_is_deleted() {
        let mut mgr = manager(40);
        mgr.write_note(&write("A", "short"), 1).unwrap();
        let error = mgr.write_note(&write("B", &"long ".repeat(60)), 2).unwrap_err();
        assert!(error.contains("notes are full"), "{error}");
        assert_eq!(mgr.partitions.task_state.notes.len(), 1);
        let error = mgr.write_note(&write("A", &"long ".repeat(60)), 3).unwrap_err();
        assert!(error.contains("notes are full"), "{error}");
        assert_eq!(mgr.partitions.task_state.notes[0].body, "short");
    }

    #[test]
    fn links_and_retirement_become_edges() {
        let mut mgr = manager(500);
        mgr.write_note(&write("Rules", "skip ids ending in 0"), 1).unwrap();
        let ack = mgr
            .write_note(&NoteRequest { links: vec!["Rules".into(), "Missing".into()], ..write("Tally", "2") }, 2)
            .unwrap();
        assert!(ack.contains("Missing"), "{ack}");
        mgr.write_note(&write("Tally v2", "3"), 3).unwrap();
        let retire = NoteRequest { action: NoteAction::Retire, replaced_by: Some("Tally v2".into()), ..NoteRequest { title: "Tally".into(), ..NoteRequest::default() } };
        mgr.write_note(&retire, 4).unwrap();
        assert_eq!(mgr.partitions.task_state.notes_retired, 1);
        assert_eq!(edges(&mgr, Relation::LinksTo).len(), 1);
        assert_eq!(edges(&mgr, Relation::Supersedes).len(), 1);
        assert!(mgr.write_note(&NoteRequest { action: NoteAction::Retire, ..NoteRequest { title: "Tally".into(), ..NoteRequest::default() } }, 5).is_err());
    }

    fn reading_manager(reads_notes: bool, notice: bool) -> ContextManager {
        let mut mgr = ContextManager::with_config(
            100_000,
            ContextConfig {
                notes_budget_tokens: 800,
                compaction_reads_notes: reads_notes,
                page_out_notice: notice,
                ..ContextConfig::default()
            },
            ContextTokenEngine::char_approx(),
        );
        mgr.init_task("count the errors".into(), vec![]);
        for n in 0..6 {
            read(&mut mgr, &format!("c{n}"), &format!("logs/{n}.log"), &format!("{n} ").repeat(200));
        }
        mgr
    }

    fn archived_calls(archived: &[CoreMessage]) -> Vec<String> {
        archived
            .iter()
            .filter_map(|m| match &m.content {
                Content::Parts(parts) => Some(parts.iter().filter_map(|p| match p {
                    ContentPart::ToolResult { call_id, .. } => Some(call_id.to_string()),
                    _ => None,
                }).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Just under the current total: the compaction has to archive exactly one unit.
    fn one_unit_target(mgr: &ContextManager) -> u32 {
        mgr.partitions.total_tokens(&mgr.engine) - 20
    }

    #[test]
    fn with_compaction_reading_notes_the_cited_result_leaves_first() {
        for (reads, expected) in [(false, "c0"), (true, "c2")] {
            let mut mgr = reading_manager(reads, false);
            let cite = NoteRequest { evidence: vec!["c2".into()], ..write("File 2", "2 errors") };
            mgr.write_note(&cite, 1).unwrap();
            let target = one_unit_target(&mgr);
            let (_, summary, archived, _) = mgr.compress_with_target(PressureAction::ContextCollapse, target, None);
            assert_eq!(archived_calls(&archived), vec![expected.to_string()], "reads_notes={reads}");
            let state = &mgr.partitions.task_state;
            if reads {
                assert_eq!(state.notes[0].archived, vec!["c2".to_string()]);
                assert!(state.format_compact().contains("(archived evidence: c2 → read_result)"));
                assert!(summary.unwrap().starts_with("archived 1 turn(s)"));
            } else {
                assert!(state.notes[0].archived.is_empty());
            }
        }
    }

    #[test]
    fn a_page_out_notice_defers_one_compaction_and_lists_uncited_results() {
        let mut mgr = reading_manager(true, true);
        mgr.write_note(&NoteRequest { evidence: vec!["c2".into()], ..write("File 2", "2 errors") }, 1).unwrap();
        // Everything but the preserved tail must go: c0, c1, c3 are uncited, c2 is cited.
        let target = 10;
        assert!(mgr.defer_for_page_out_notice(target));
        let notice = mgr.partitions.task_state.page_out_notice.clone();
        assert!(notice.iter().any(|line| line.starts_with("c0 read_file(")), "{notice:?}");
        assert!(!notice.iter().any(|line| line.starts_with("c2 ")), "{notice:?}");
        assert!(mgr.partitions.task_state.format_compact().contains("leaving context at the next compaction"));
        // The next turn the compaction runs, and the notice is spent.
        assert!(!mgr.defer_for_page_out_notice(target));
        let (_, _, archived, _) = mgr.compress_with_target(PressureAction::ContextCollapse, target, None);
        assert!(!archived.is_empty());
        assert!(mgr.partitions.task_state.page_out_notice.is_empty());
    }

    #[test]
    fn without_the_notice_switch_nothing_is_deferred() {
        let mut mgr = reading_manager(true, false);
        assert!(!mgr.defer_for_page_out_notice(10));
        assert!(mgr.partitions.task_state.page_out_notice.is_empty());
    }

    #[test]
    fn notes_survive_renewal() {
        let mut mgr = manager(500);
        mgr.write_note(&write("Tally", "6 files read, 4 errors"), 1).unwrap();
        mgr.renew();
        assert_eq!(mgr.partitions.task_state.notes.len(), 1);
        assert!(mgr.partitions.task_state.format_compact().contains("6 files read, 4 errors"));
    }

    #[test]
    fn notes_survive_compaction_of_the_history_they_came_from() {
        let mut mgr = manager(500);
        for n in 0..6 {
            read(&mut mgr, &format!("c{n}"), &format!("logs/{n}.log"), &"line ".repeat(400));
        }
        mgr.write_note(&write("Tally", "6 files read, 4 errors"), 1).unwrap();
        let note_item = mgr.partitions.task_state.notes[0].item;
        mgr.partitions.history.clear();
        assert!(mgr.partitions.task_state.format_compact().contains("6 files read, 4 errors"));
        assert!(mgr.resident_item_ids_for_test().contains(&note_item));
    }
}
