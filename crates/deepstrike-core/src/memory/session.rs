use std::collections::VecDeque;

use crate::types::message::Message;

#[derive(Debug, Clone, Default)]
pub enum RestorePolicy {
    #[default]
    None,
    TranscriptOnly,
    Window,
    RuntimeState,
}

#[derive(Debug, Clone)]
pub struct RestoreConfig {
    pub max_messages: usize,
    pub max_chars: usize,
    pub include_context: bool,
    pub include_events: bool,
}

impl Default for RestoreConfig {
    fn default() -> Self {
        Self {
            max_messages: 20,
            max_chars: 8000,
            include_context: true,
            include_events: false,
        }
    }
}

/// Apply `policy` to `messages` and return the subset to inject at run start.
pub fn restore(
    policy: &RestorePolicy,
    config: &RestoreConfig,
    messages: &[Message],
) -> Vec<Message> {
    match policy {
        RestorePolicy::None => vec![],
        RestorePolicy::TranscriptOnly => messages.to_vec(),
        RestorePolicy::Window => {
            let mut result: Vec<Message> = Vec::new();
            let mut chars = 0usize;
            for msg in messages.iter().rev() {
                let len = msg.content.text_len();
                if result.len() >= config.max_messages || chars + len > config.max_chars {
                    break;
                }
                result.push(msg.clone());
                chars += len;
            }
            result.reverse();
            result
        }
        RestorePolicy::RuntimeState => messages.to_vec(),
    }
}

/// Session memory: message history that persists across runs within a session.
#[derive(Debug)]
pub struct SessionMemory {
    messages: VecDeque<Message>,
    pub max_messages: usize,
    pub max_tokens: u32,
    current_tokens: u32,
}

impl Default for SessionMemory {
    fn default() -> Self {
        Self::new(100, u32::MAX)
    }
}

impl SessionMemory {
    pub fn new(max_messages: usize, max_tokens: u32) -> Self {
        Self {
            messages: VecDeque::new(),
            max_messages,
            max_tokens,
            current_tokens: 0,
        }
    }

    pub fn push(&mut self, msg: Message) {
        let tokens = msg.token_count.unwrap_or(0);
        self.messages.push_back(msg);
        self.current_tokens += tokens;
        while (self.messages.len() > self.max_messages || self.current_tokens > self.max_tokens)
            && !self.messages.is_empty()
        {
            let removed = self.messages.pop_front().unwrap();
            self.current_tokens = self
                .current_tokens
                .saturating_sub(removed.token_count.unwrap_or(0));
        }
    }

    pub fn token_count(&self) -> u32 {
        self.current_tokens
    }

    /// Returns messages as a slice for read-only access.
    pub fn as_slice(&self) -> Vec<&Message> {
        self.messages.iter().collect()
    }

    pub fn recent(&self, n: usize) -> Vec<&Message> {
        let start = self.messages.len().saturating_sub(n);
        self.messages.iter().skip(start).collect()
    }

    pub fn to_vec(&self) -> Vec<Message> {
        self.messages.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.current_tokens = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest_when_full() {
        let mut mem = SessionMemory::new(2, 10000);
        let mut m1 = Message::user("first");
        m1.token_count = Some(10);
        let mut m2 = Message::user("second");
        m2.token_count = Some(10);
        let mut m3 = Message::user("third");
        m3.token_count = Some(10);

        mem.push(m1);
        mem.push(m2);
        mem.push(m3);

        let msgs = mem.to_vec();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content.as_text().unwrap(), "second");
    }
}
