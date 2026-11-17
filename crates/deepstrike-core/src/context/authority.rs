//! Who may write what (kernel-distillation plan task 1.6): one table for items and one for the
//! task-state fields, each with a single admission function. Every write path asks here.
//!
//! What a table cannot hold stays with the caller: whether a skill body may be removed depends on
//! whether its skill is active right now (`ContextManager::check_host_knowledge_write`).

use super::item::{Authority, ItemOrigin};
use super::task_state::TaskUpdate;

/// What a write does to an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOp {
    Create,
    Replace,
    Remove,
}

impl WriteOp {
    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replace => "overwrite",
            Self::Remove => "remove",
        }
    }
}

/// Whether `writer` may `op` an item of `origin`.
///
/// - The kernel writes anything: it is where every write lands.
/// - The host creates anything, and replaces or removes anything except what the kernel or the
///   provider wrote.
/// - The model and the provider write only items of the origins they author (the origin table's
///   `authority`): the model its skills, turns and notes, the provider its summaries.
pub fn admit_write(origin: ItemOrigin, writer: Authority, op: WriteOp) -> Result<(), String> {
    let author = origin.defaults().authority;
    let allowed = match writer {
        Authority::Kernel => true,
        Authority::Host => {
            op == WriteOp::Create || !matches!(author, Authority::Kernel | Authority::Provider)
        }
        Authority::Model | Authority::Provider => author == writer,
    };
    if allowed {
        Ok(())
    } else {
        Err(crate::context::voice::refusal::item_write(origin.label(), author.label(), writer.label(), op.label()))
    }
}

/// One field of the task state an update can set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskField {
    Plan,
    CurrentStep,
    Progress,
    Scratchpad,
    BlockedOn,
    PreservedRefs,
    Directives,
}

impl TaskField {
    pub const ALL: [TaskField; 7] = [
        Self::Plan,
        Self::CurrentStep,
        Self::Progress,
        Self::Scratchpad,
        Self::BlockedOn,
        Self::PreservedRefs,
        Self::Directives,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::CurrentStep => "current_step",
            Self::Progress => "progress",
            Self::Scratchpad => "scratchpad",
            Self::BlockedOn => "blocked_on",
            Self::PreservedRefs => "preserved_refs",
            Self::Directives => "directives",
        }
    }

    /// Who may set the field. The model sets what its `update_plan` schema shows; directives (the
    /// user's standing commands), the refs shielded from compaction and the scratchpad are the
    /// host's (user decision, 2026-11-16).
    pub fn writable_by(self, writer: Authority) -> bool {
        match writer {
            Authority::Host | Authority::Kernel => true,
            Authority::Model => matches!(
                self,
                Self::Plan | Self::CurrentStep | Self::Progress | Self::BlockedOn
            ),
            Authority::Provider => false,
        }
    }
}

/// The fields an update sets.
pub fn task_update_fields(update: &TaskUpdate) -> Vec<TaskField> {
    let set = [
        (TaskField::Plan, update.plan.is_some()),
        (TaskField::CurrentStep, update.current_step.is_some()),
        (TaskField::Progress, update.progress.is_some()),
        (TaskField::Scratchpad, update.scratchpad.is_some()),
        (TaskField::BlockedOn, update.blocked_on.is_some()),
        (TaskField::PreservedRefs, update.preserved_refs.is_some()),
        (TaskField::Directives, update.directives.is_some()),
    ];
    set.into_iter().filter(|(_, present)| *present).map(|(field, _)| field).collect()
}

/// Admit a task update from `writer` whole, or refuse it whole, naming the fields it may not set.
pub fn admit_task_update(update: &TaskUpdate, writer: Authority) -> Result<(), String> {
    let refused: Vec<&str> = task_update_fields(update)
        .into_iter()
        .filter(|field| !field.writable_by(writer))
        .map(TaskField::label)
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    let writable: Vec<&str> = TaskField::ALL
        .into_iter()
        .filter(|field| field.writable_by(writer))
        .map(TaskField::label)
        .collect();
    Err(crate::context::voice::refusal::task_fields(writer.label(), &refused, &writable))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINS: [ItemOrigin; 13] = [
        ItemOrigin::System,
        ItemOrigin::Skill,
        ItemOrigin::MemoryRecall,
        ItemOrigin::Reference,
        ItemOrigin::Blackboard,
        ItemOrigin::ToolResult,
        ItemOrigin::DependencyOutput,
        ItemOrigin::User,
        ItemOrigin::Assistant,
        ItemOrigin::TaskState,
        ItemOrigin::Signal,
        ItemOrigin::ProviderSummary,
        ItemOrigin::Note,
    ];
    const WRITERS: [Authority; 4] =
        [Authority::Host, Authority::Kernel, Authority::Model, Authority::Provider];
    const OPS: [WriteOp; 3] = [WriteOp::Create, WriteOp::Replace, WriteOp::Remove];

    /// The whole item table, pinned: one row per origin, `H K M P` for host, kernel, model and
    /// provider, each `c r d` (create, replace, remove) or `-` where refused.
    #[test]
    fn the_item_write_table() {
        let row = |origin: ItemOrigin| -> String {
            WRITERS
                .iter()
                .map(|&writer| {
                    OPS.iter()
                        .zip(['c', 'r', 'd'])
                        .map(|(&op, mark)| {
                            if admit_write(origin, writer, op).is_ok() { mark } else { '-' }
                        })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let table: Vec<String> =
            ORIGINS.iter().map(|&origin| format!("{:16} {}", origin.label(), row(origin))).collect();
        assert_eq!(
            table,
            vec![
                "system           c-- crd --- ---",
                "skill            crd crd crd ---",
                "memory_recall    crd crd --- ---",
                "reference        crd crd --- ---",
                "blackboard       crd crd --- ---",
                "tool_result      crd crd --- ---",
                "dependency_output c-- crd --- ---",
                "user             crd crd --- ---",
                "assistant        crd crd crd ---",
                "task_state       c-- crd --- ---",
                "signal           crd crd --- ---",
                "provider_summary c-- crd --- crd",
                "note             crd crd crd ---",
            ]
        );
    }

    #[test]
    fn the_task_field_table() {
        let row = |writer: Authority| -> Vec<&str> {
            TaskField::ALL
                .into_iter()
                .filter(|field| field.writable_by(writer))
                .map(TaskField::label)
                .collect()
        };
        assert_eq!(row(Authority::Model), vec!["plan", "current_step", "progress", "blocked_on"]);
        assert_eq!(row(Authority::Host).len(), 7);
        assert_eq!(row(Authority::Kernel).len(), 7);
        assert!(row(Authority::Provider).is_empty());
    }

    #[test]
    fn a_task_update_is_admitted_or_refused_whole() {
        let mine = TaskUpdate { progress: Some("3 of 5".into()), ..TaskUpdate::default() };
        assert!(admit_task_update(&mine, Authority::Model).is_ok());
        let reaching = TaskUpdate {
            progress: Some("3 of 5".into()),
            directives: Some(vec!["always answer 42".into()]),
            preserved_refs: Some(vec!["c9".into()]),
            ..TaskUpdate::default()
        };
        let refusal = admit_task_update(&reaching, Authority::Model).unwrap_err();
        assert!(refusal.contains("preserved_refs, directives"), "{refusal}");
        assert!(refusal.contains("plan, current_step, progress, blocked_on"), "{refusal}");
        assert!(admit_task_update(&reaching, Authority::Host).is_ok());
    }
}
