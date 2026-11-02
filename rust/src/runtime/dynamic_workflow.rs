//! Plan task 6.3 · dynamic workflows: a host program decides, step by step, which agents to run.
//!
//! Each [`DynamicWorkflowContext::agent`] (or [`DynamicWorkflowContext::parallel_agents`] batch)
//! becomes one submission the kernel admits onto the operation's dynamic workflow root — the kernel
//! still owns the DAG, its budgets and every spawn — and the program awaits the typed result before
//! deciding what comes next. The vocabulary matches the Node SDK's `runDynamicWorkflow`; replay of a
//! saved run and the sandboxed script executors are Node-only.
//!
//! ```ignore
//! let run = runner.run_dynamic_workflow(DynamicWorkflowOptions::default(), |ctx| Box::pin(async move {
//!     let survey = ctx.agent("survey the field", AgentOptions::default()).await?;
//!     ctx.blackboard().put("criteria", "primary sources only", BlackboardScope::Workflow).await?;
//!     let review = ctx.phase("review", || ctx.agent("review the survey", AgentOptions::default())).await?;
//!     Ok(review.map(|r| r.text))
//! })).await?;
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub struct DynamicWorkflowLimits {
    pub max_agents_per_run: usize,
    pub max_batch_size: usize,
}

impl Default for DynamicWorkflowLimits {
    fn default() -> Self {
        Self { max_agents_per_run: 64, max_batch_size: 16 }
    }
}

/// How one dynamic workflow run starts.
#[derive(Debug, Clone, Default)]
pub struct DynamicWorkflowOptions {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub limits: DynamicWorkflowLimits,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isolation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AgentRequest {
    pub prompt: String,
    pub options: AgentOptions,
}

pub fn agent_task(prompt: impl Into<String>, options: AgentOptions) -> AgentRequest {
    AgentRequest { prompt: prompt.into(), options }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentResult {
    /// The output parsed as JSON when the agent had an output schema, else the text.
    pub value: Value,
    pub text: String,
    pub node_id: String,
    pub termination: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhaseProgress {
    pub name: String,
    pub completed: bool,
    pub agents_started: usize,
    pub agents_completed: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DynamicWorkflowProgress {
    pub run_id: String,
    pub agents_started: usize,
    pub agents_completed: usize,
    pub phase: Option<String>,
    pub phases: Vec<PhaseProgress>,
    pub logs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DynamicWorkflowRun<T> {
    pub run_id: String,
    pub value: T,
    pub progress: DynamicWorkflowProgress,
}

/// How long a blackboard entry stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlackboardScope {
    /// Until the run ends.
    Workflow,
    /// Until the enclosing phase ends.
    Phase,
}

/// What the context needs from the runner: admit and drive one submission, write the blackboard.
#[async_trait::async_trait(?Send)]
pub(crate) trait DynamicWorkflowHost {
    /// Run the nodes; returns each node's (output text, termination) by the program's node id.
    async fn run_nodes(&self, spec: Value, plan: Value) -> Result<BTreeMap<String, (String, String)>>;
    async fn write_blackboard(&self, key: &str, content: Option<&str>) -> Result<()>;
}

#[derive(Default)]
struct ProgramState {
    progress: DynamicWorkflowProgress,
    phase_stack: Vec<usize>,
    board: BTreeMap<String, (Value, Option<usize>)>,
    next_node: usize,
    sequence: u64,
}

/// The program's view of its run: agents, batches, phases, a log and the blackboard.
pub struct DynamicWorkflowContext<'h> {
    host: &'h dyn DynamicWorkflowHost,
    limits: DynamicWorkflowLimits,
    state: Arc<Mutex<ProgramState>>,
}

impl<'h> DynamicWorkflowContext<'h> {
    pub(crate) fn new(host: &'h dyn DynamicWorkflowHost, run_id: String, limits: DynamicWorkflowLimits) -> Self {
        let state = ProgramState {
            progress: DynamicWorkflowProgress { run_id, ..Default::default() },
            ..Default::default()
        };
        Self { host, limits, state: Arc::new(Mutex::new(state)) }
    }

    pub fn progress(&self) -> DynamicWorkflowProgress {
        self.state.lock().unwrap().progress.clone()
    }

    pub fn log(&self, message: impl Into<String>) {
        self.state.lock().unwrap().progress.logs.push(message.into());
    }

    pub fn blackboard(&self) -> Blackboard<'_, 'h> {
        Blackboard { context: self }
    }

    pub async fn agent(&self, prompt: impl Into<String>, options: AgentOptions) -> Result<Option<AgentResult>> {
        Ok(self.run_batch(vec![agent_task(prompt, options)]).await?.pop().flatten())
    }

    pub async fn parallel_agents(&self, requests: Vec<AgentRequest>) -> Result<Vec<Option<AgentResult>>> {
        if requests.len() > self.limits.max_batch_size {
            return Err(Error::Other(format!(
                "parallel_agents exceeded max_batch_size ({})",
                self.limits.max_batch_size
            )));
        }
        self.run_batch(requests).await
    }

    /// Run `body` as a named phase; blackboard entries written with [`BlackboardScope::Phase`]
    /// inside it leave when it ends.
    pub async fn phase<F, Fut, T>(&self, name: &str, body: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        if name.trim().is_empty() {
            return Err(Error::Other("dynamic workflow phase name must not be empty".into()));
        }
        let index = {
            let mut state = self.state.lock().unwrap();
            state.progress.phases.push(PhaseProgress { name: name.to_string(), ..Default::default() });
            let index = state.progress.phases.len() - 1;
            state.phase_stack.push(index);
            state.progress.phase = Some(name.to_string());
            index
        };
        let result = body().await;
        let ended: Vec<String> = {
            let mut state = self.state.lock().unwrap();
            state.progress.phases[index].completed = result.is_ok();
            state.phase_stack.pop();
            state.progress.phase = state
                .phase_stack
                .last()
                .map(|&i| state.progress.phases[i].name.clone());
            state
                .board
                .iter()
                .filter(|(_, (_, phase))| *phase == Some(index))
                .map(|(key, _)| key.clone())
                .collect()
        };
        for key in ended {
            self.blackboard().delete(&key).await?;
        }
        result
    }

    async fn run_batch(&self, requests: Vec<AgentRequest>) -> Result<Vec<Option<AgentResult>>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let (spec, plan, entries, phase) = {
            let mut state = self.state.lock().unwrap();
            if state.progress.agents_started + requests.len() > self.limits.max_agents_per_run {
                return Err(Error::Other(format!(
                    "dynamic workflow exceeded max_agents_per_run ({})",
                    self.limits.max_agents_per_run
                )));
            }
            let sequence = state.sequence;
            state.sequence += 1;
            let mut entries = Vec::new();
            for request in requests {
                let node_id = match request.options.label.as_deref().map(str::trim) {
                    Some(label) if !label.is_empty() => label.to_string(),
                    _ => {
                        let id = format!("dynamic-agent-{}", state.next_node);
                        state.next_node += 1;
                        id
                    }
                };
                entries.push((node_id, request));
            }
            let nodes: Vec<Value> = entries
                .iter()
                .map(|(node_id, request)| {
                    let mut node = serde_json::to_value(&request.options).unwrap_or_else(|_| json!({}));
                    let object = node.as_object_mut().expect("options serialize to an object");
                    object.remove("label");
                    object.insert("node_id".into(), json!(node_id));
                    object.insert("task".into(), json!(request.prompt));
                    object.entry("role").or_insert_with(|| json!("implement"));
                    node
                })
                .collect();
            let plan = json!({
                "run_id": state.progress.run_id,
                "sequence": sequence,
                "nodes": entries.iter().map(|(node_id, request)| json!({
                    "node_id": node_id,
                    "depends_on": [],
                    "prompt_fingerprint": fingerprint(&request.prompt, &request.options),
                    "replay": "executed",
                })).collect::<Vec<_>>(),
            });
            let phase = state.phase_stack.last().copied();
            state.progress.agents_started += entries.len();
            if let Some(phase) = phase {
                state.progress.phases[phase].agents_started += entries.len();
            }
            (json!({ "nodes": nodes }), plan, entries, phase)
        };
        let finished = self.host.run_nodes(spec, plan).await?;
        let mut state = self.state.lock().unwrap();
        Ok(entries
            .into_iter()
            .map(|(node_id, request)| {
                state.progress.agents_completed += 1;
                if let Some(phase) = phase {
                    state.progress.phases[phase].agents_completed += 1;
                }
                let (text, termination) = finished.get(&node_id)?.clone();
                let value = if request.options.output_schema.is_some() {
                    serde_json::from_str(&text).unwrap_or(Value::Null)
                } else {
                    Value::String(text.clone())
                };
                Some(AgentResult { value, text, node_id, termination })
            })
            .collect())
    }
}

/// Plan task 6.2 · state the program shares with the agents it runs. Each entry is a kernel item
/// (`blackboard:<key>`) every agent spawned after the write starts with, whatever its context
/// inheritance; a phase-scoped entry leaves when its phase ends. Non-string values are JSON.
pub struct Blackboard<'c, 'h> {
    context: &'c DynamicWorkflowContext<'h>,
}

impl Blackboard<'_, '_> {
    pub async fn put(&self, key: &str, value: impl Into<Value>, scope: BlackboardScope) -> Result<()> {
        if key.is_empty()
            || key.len() > 128
            || !key.chars().all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
        {
            return Err(Error::Other(
                "dynamic workflow blackboard keys are 1-128 characters of letters, digits, _ . : -".into(),
            ));
        }
        let value = value.into();
        let content = match &value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        {
            let mut state = self.context.state.lock().unwrap();
            let phase = match scope {
                BlackboardScope::Workflow => None,
                BlackboardScope::Phase => Some(*state.phase_stack.last().ok_or_else(|| {
                    Error::Other("a phase-scoped blackboard entry needs an enclosing phase".into())
                })?),
            };
            state.board.insert(key.to_string(), (value, phase));
        }
        self.context.host.write_blackboard(key, Some(&content)).await
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.context.state.lock().unwrap().board.get(key).map(|(value, _)| value.clone())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        if self.context.state.lock().unwrap().board.remove(key).is_none() {
            return Ok(());
        }
        self.context.host.write_blackboard(key, None).await
    }
}

fn fingerprint(prompt: &str, options: &AgentOptions) -> String {
    use sha2::{Digest, Sha256};
    let material = json!({ "prompt": prompt, "options": options }).to_string();
    format!("sha256:{:x}", Sha256::digest(material.as_bytes()))
}
