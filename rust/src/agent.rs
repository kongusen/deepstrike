//! Public executable Agent facade.
//!
//! `AgentDefinition` is the serializable semantic contract. `RuntimeOptions` is the host-owned
//! binding and is consumed when the executable `Agent` is created; the definition never stores
//! provider, session, or execution-plane authority.
// Canonical cross-SDK fields: capabilityFilter, mcpServers, providerOptions, outputSchema.

use crate::RunEvent;
use crate::runtime::{RuntimeOptions, RuntimeRunner};
use crate::{Error, Result};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AgentDefinition {
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub model: Option<serde_json::Value>,
    /// Capability and semantic fields are JSON values so the Rust facade can carry the same
    /// declaration produced by the JSON-oriented SDKs without importing their host types.
    pub capability_filter: Option<serde_json::Value>,
    pub tools: Option<Vec<serde_json::Value>>,
    pub mcp_servers: Option<Vec<serde_json::Value>>,
    pub skills: Option<Vec<serde_json::Value>>,
    pub memory: Option<serde_json::Value>,
    pub knowledge: Option<Vec<serde_json::Value>>,
    pub handoffs: Option<Vec<serde_json::Value>>,
    pub provider_options: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub metadata: Option<serde_json::Value>,
    pub guardrails: Option<Vec<serde_json::Value>>,
}

impl AgentDefinition {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// Executable public Agent handle. Host runtime authority is bound once at construction.
pub struct Agent {
    definition: AgentDefinition,
    runner: RuntimeRunner,
    session_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PortableRunResult {
    pub output: String,
    pub session_id: String,
    pub status: String,
}
pub type AgentRunResult = PortableRunResult;

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
        let session_id = binding
            .session_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        binding.session_id = Some(session_id.clone());
        Ok(Self {
            definition,
            runner: RuntimeRunner::new(binding),
            session_id,
        })
    }

    pub fn definition(&self) -> &AgentDefinition {
        &self.definition
    }
    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub async fn run(&self, goal: &str) -> Result<AgentRunResult> {
        let output = self.runner.execute(goal).await?;
        Ok(AgentRunResult {
            output,
            session_id: self.session_id.clone(),
            status: "completed".into(),
        })
    }

    pub async fn stream<'a>(
        &'a self,
        goal: &'a str,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<RunEvent>> + 'a>>> {
        self.runner.run_streaming(goal, &[], None, None).await
    }
}

/// Construct the portable executable Agent contract from a semantic definition and host binding.
pub fn create_agent(definition: AgentDefinition, binding: RuntimeOptions) -> Result<Agent> {
    Agent::bind(definition, binding)
}
