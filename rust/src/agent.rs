//! High-level Rust agent/session facade.
//!
//! `RuntimeRunner` remains the fully configurable kernel driver.  This module provides the
//! stable object model used by the other SDKs while keeping Rust ownership and streaming idioms:
//! an [`Agent`] owns one runner, and an [`AgentSession`] gives that runner a durable session id.

use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::run_event::RunEvent;
use crate::runtime::session_log::SessionEntry;
use crate::{Result, RuntimeOptions, RuntimeRunner};

/// A completed run projected from the runtime event stream.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentRunResult {
    pub text: String,
    pub run_id: Option<String>,
    pub status: String,
    pub iterations: u32,
    pub total_tokens: u64,
}

/// A high-level agent backed by the canonical Rust runtime.
pub struct Agent {
    runner: Arc<RuntimeRunner>,
}

impl Agent {
    pub fn new(options: RuntimeOptions) -> Self {
        Self {
            runner: Arc::new(RuntimeRunner::new(options)),
        }
    }

    pub fn with_runner(runner: RuntimeRunner) -> Self {
        Self {
            runner: Arc::new(runner),
        }
    }

    pub fn runner(&self) -> &RuntimeRunner {
        &self.runner
    }

    pub fn session(&self, session_id: impl Into<String>) -> AgentSession {
        AgentSession {
            runner: Arc::clone(&self.runner),
            session_id: session_id.into(),
        }
    }

    pub async fn run(&self, goal: &str) -> Result<AgentRunResult> {
        self.session(uuid::Uuid::new_v4().to_string())
            .run(goal)
            .await
    }

    pub async fn stream<'a>(&'a self, goal: &'a str) -> Result<AgentStream<'a>> {
        self.runner.run_streaming(goal, &[], None, None).await
    }
}

/// A durable, resumable session handle.
#[derive(Clone)]
pub struct AgentSession {
    runner: Arc<RuntimeRunner>,
    session_id: String,
}

pub type AgentStream<'a> = Pin<Box<dyn Stream<Item = Result<RunEvent>> + 'a>>;

impl AgentSession {
    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub async fn run(&self, goal: &str) -> Result<AgentRunResult> {
        let mut stream = self.stream(goal).await?;
        let mut result = AgentRunResult::default();
        while let Some(event) = stream.next().await {
            match event? {
                RunEvent::TextDelta(delta) => result.text.push_str(&delta),
                RunEvent::Done {
                    iterations,
                    total_tokens,
                    status,
                } => {
                    result.iterations = iterations;
                    result.total_tokens = total_tokens;
                    result.status = status;
                }
                _ => {}
            }
        }
        result.run_id = self.latest_run_id().await?;
        Ok(result)
    }

    pub async fn stream<'a>(&'a self, goal: &'a str) -> Result<AgentStream<'a>> {
        self.runner
            .run_streaming(goal, &[], None, Some(&self.session_id))
            .await
    }

    pub async fn resume<'a>(&'a self) -> Result<AgentStream<'a>> {
        self.runner.wake_streaming(&self.session_id, None).await
    }

    pub async fn history(&self) -> Result<Vec<SessionEntry>> {
        self.runner.read_session(&self.session_id).await
    }

    pub async fn latest_seq(&self) -> Result<i64> {
        self.runner.latest_session_seq(&self.session_id).await
    }

    pub fn interrupt(&self) {
        self.runner.interrupt();
    }

    async fn latest_run_id(&self) -> Result<Option<String>> {
        Ok(self.history().await?.into_iter().rev().find_map(|entry| {
            if let deepstrike_core::runtime::session::SessionEvent::RunStarted { run_id, .. } =
                entry.event
            {
                Some(run_id)
            } else {
                None
            }
        }))
    }
}
