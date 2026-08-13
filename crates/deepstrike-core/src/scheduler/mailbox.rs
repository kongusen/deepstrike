//! spc_006: Task-to-task IPC — point-to-point [`Mailbox`] (this card + spc_006-02/03) and
//! many-to-one/one-to-many [`Channel`] (spc_006-04). Additive-only in this card: the message
//! shape only, no send/receive, no wiring onto [`super::tcb::Tcb`].
//!
//! Naming note: spc_006 §3 names this struct `Message`, but `crate::types::message::Message`
//! (the LLM conversation message) already owns that name and is glob-imported (`use super::*`)
//! into `scheduler::state_machine::tests` — a second unqualified `Message` there would force
//! every reference in this module's own integration tests to be fully qualified. `MailboxMessage`
//! avoids the collision while staying unambiguous about what it is.

use std::collections::{HashMap, VecDeque};

use compact_str::CompactString;
use serde::{Deserialize, Serialize};

use super::tcb::TaskId;
use crate::mm::handle::HandleId;
use crate::types::signal::Urgency;

/// Opaque message id — mirrors [`TaskId`]'s convention of a plain `CompactString` alias rather
/// than a validated newtype (no producer needs anything richer yet).
pub type MessageId = CompactString;

/// spc_006: no existing "logical time" counter type exists to reuse (`LoopStateMachine::turn` is
/// a private `u32` field, not a public type) — a minimal placeholder newtype, following the same
/// convention [`super::tcb::LogicalDeadline`] established in spc_003-02 for concepts with no
/// producer yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LogicalTime(pub u32);

/// A point-to-point message between two tasks. Large payloads never live inline — `payload_handle`
/// points into the sender's [`crate::mm::handle::HandleTable`] (spc_006 §5: pass handles, not
/// prompts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxMessage {
    pub id: MessageId,
    pub from: TaskId,
    pub to: TaskId,
    pub kind: CompactString,
    pub payload_handle: HandleId,
    pub priority: Urgency,
    pub timestamp: LogicalTime,
}

/// spc_006-02: a task's inbox — point-to-point only (no fan-out; that's [`Channel`]'s job,
/// spc_006-04). Pure data structure: no `TaskTable`/`Tcb` reference, no send/receive wiring onto
/// a real task yet (spc_006-03).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mailbox {
    queue: VecDeque<MailboxMessage>,
}

impl Mailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn send(&mut self, msg: MailboxMessage) {
        self.queue.push_back(msg);
    }

    /// FIFO — oldest message first.
    pub fn receive(&mut self) -> Option<MailboxMessage> {
        self.queue.pop_front()
    }
}

/// spc_006-04: many-to-one / one-to-many fan-in. Unlike [`Mailbox`], a `publish`ed message is
/// never removed from the shared `buffer` — each subscriber reads independently via its own
/// cursor into that buffer (the doc's "共享 buffer + per-consumer 已读游标" design; the `cursors`
/// field is not shown in spc_006 §3's abbreviated struct sketch but is mechanically required to
/// realize that description — without it a second consumer draining after a first would see
/// nothing).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Channel {
    pub subscribers: Vec<TaskId>,
    buffer: VecDeque<MailboxMessage>,
    #[serde(default)]
    cursors: HashMap<TaskId, usize>,
}

impl Channel {
    pub fn new(subscribers: Vec<TaskId>) -> Self {
        Self {
            subscribers,
            buffer: VecDeque::new(),
            cursors: HashMap::new(),
        }
    }

    pub fn publish(&mut self, msg: MailboxMessage) {
        self.buffer.push_back(msg);
    }

    /// Every message published since `consumer`'s last `drain_for`, oldest first; advances that
    /// consumer's cursor to the current end of the buffer. Independent of every other consumer's
    /// cursor — draining does not consume the buffer.
    pub fn drain_for(&mut self, consumer: TaskId) -> Vec<MailboxMessage> {
        let cursor = self.cursors.entry(consumer).or_insert(0);
        let unread: Vec<MailboxMessage> = self.buffer.iter().skip(*cursor).cloned().collect();
        *cursor = self.buffer.len();
        unread
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spc_006_01_mailbox_message_fields_are_readable() {
        let msg = MailboxMessage {
            id: MessageId::from("msg-1"),
            from: TaskId::from("a"),
            to: TaskId::from("b"),
            kind: CompactString::from("research_result"),
            payload_handle: 42,
            priority: Urgency::High,
            timestamp: LogicalTime(7),
        };

        assert_eq!(msg.id, MessageId::from("msg-1"));
        assert_eq!(msg.from, TaskId::from("a"));
        assert_eq!(msg.to, TaskId::from("b"));
        assert_eq!(msg.kind, CompactString::from("research_result"));
        assert_eq!(msg.payload_handle, 42);
        assert_eq!(msg.priority, Urgency::High);
        assert_eq!(msg.timestamp, LogicalTime(7));
    }

    fn msg(id: &str) -> MailboxMessage {
        MailboxMessage {
            id: MessageId::from(id),
            from: TaskId::from("a"),
            to: TaskId::from("b"),
            kind: CompactString::from("kind"),
            payload_handle: 1,
            priority: Urgency::Normal,
            timestamp: LogicalTime(0),
        }
    }

    #[test]
    fn spc_006_02_receive_on_an_empty_mailbox_returns_none() {
        let mut mailbox = Mailbox::new();
        assert_eq!(mailbox.receive(), None);
    }

    #[test]
    fn spc_006_02_receive_returns_sent_messages_in_fifo_order() {
        let mut mailbox = Mailbox::new();
        mailbox.send(msg("first"));
        mailbox.send(msg("second"));

        assert_eq!(
            mailbox.receive().map(|m| m.id),
            Some(MessageId::from("first"))
        );
        assert_eq!(
            mailbox.receive().map(|m| m.id),
            Some(MessageId::from("second"))
        );
    }

    #[test]
    fn spc_006_02_receive_returns_none_once_drained() {
        let mut mailbox = Mailbox::new();
        mailbox.send(msg("only"));
        assert!(mailbox.receive().is_some());
        assert_eq!(mailbox.receive(), None);
    }

    fn msg_from(id: &str, from: &str) -> MailboxMessage {
        MailboxMessage {
            id: MessageId::from(id),
            from: TaskId::from(from),
            to: TaskId::from("coordinator"),
            kind: CompactString::from("kind"),
            payload_handle: 1,
            priority: Urgency::Normal,
            timestamp: LogicalTime(0),
        }
    }

    #[test]
    fn spc_006_04_drain_for_gathers_every_producers_message_in_arrival_order() {
        let mut channel = Channel::new(vec![TaskId::from("coordinator")]);
        channel.publish(msg_from("m1", "worker-1"));
        channel.publish(msg_from("m2", "worker-2"));
        channel.publish(msg_from("m3", "worker-3"));

        let drained = channel.drain_for(TaskId::from("coordinator"));
        let ids: Vec<_> = drained.iter().map(|m| m.id.clone()).collect();
        assert_eq!(
            ids,
            vec![
                MessageId::from("m1"),
                MessageId::from("m2"),
                MessageId::from("m3"),
            ]
        );
    }

    #[test]
    fn spc_006_04_drain_for_only_returns_messages_published_since_the_last_drain() {
        let mut channel = Channel::new(vec![TaskId::from("coordinator")]);
        channel.publish(msg_from("m1", "worker-1"));
        assert_eq!(channel.drain_for(TaskId::from("coordinator")).len(), 1);
        assert_eq!(
            channel.drain_for(TaskId::from("coordinator")),
            Vec::new(),
            "a second drain with nothing new published must come back empty"
        );

        channel.publish(msg_from("m2", "worker-2"));
        let second_batch = channel.drain_for(TaskId::from("coordinator"));
        assert_eq!(second_batch.len(), 1);
        assert_eq!(second_batch[0].id, MessageId::from("m2"));
    }

    #[test]
    fn spc_006_04_two_consumers_drain_independently_from_the_same_buffer() {
        let mut channel = Channel::new(vec![TaskId::from("c1"), TaskId::from("c2")]);
        channel.publish(msg_from("m1", "worker-1"));

        let c1_drained = channel.drain_for(TaskId::from("c1"));
        assert_eq!(c1_drained.len(), 1);

        // c2 has never drained yet — it must still see m1, not have it "stolen" by c1's read.
        let c2_drained = channel.drain_for(TaskId::from("c2"));
        assert_eq!(c2_drained.len(), 1);
        assert_eq!(c2_drained[0].id, MessageId::from("m1"));
    }
}
