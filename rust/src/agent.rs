//! Public executable Agent facade.
//!
//! `AgentDefinition` is the serializable semantic contract. `RuntimeOptions` is the host-owned
//! binding and is consumed when the executable `Agent` is created; the definition never stores
//! provider, session, or execution-plane authority.

use crate::{Error, Result};
use crate::runtime::{RuntimeOptions, RuntimeRunner};
use crate::RunEvent;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub model: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

impl AgentDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }
}

/// Executable public Agent handle. Host runtime authority is bound once at construction.
pub struct Agent {
    definition: AgentDefinition,
    runner: RuntimeRunner,
}

impl Agent {
    pub fn bind(definition: AgentDefinition, mut binding: RuntimeOptions) -> Result<Self> {
        if definition.name.trim().is_empty() {
            return Err(Error::Tool("agent name is required".into()));
        }
        if binding.system_prompt.is_none() {
            binding.system_prompt = definition.instructions.clone();
        }
        if binding.agent_id.is_none() {
            binding.agent_id = Some(definition.name.clone());
        }
        Ok(Self { definition, runner: RuntimeRunner::new(binding) })
    }

    pub fn definition(&self) -> &AgentDefinition { &self.definition }
    pub fn name(&self) -> &str { &self.definition.name }

    pub async fn run(&self, goal: &str) -> Result<String> {
        self.runner.execute(goal).await
    }

    pub async fn stream<'a>(
        &'a self,
        goal: &'a str,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        self.runner.run_streaming(goal, &[], None, None).await
    }
}
