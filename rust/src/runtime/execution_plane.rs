use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

use async_stream::try_stream;
use deepstrike_core::context::manager::{KNOWLEDGE_TOOL_NAME, MEMORY_TOOL_NAME};
use deepstrike_core::context::skill_catalog::SKILL_TOOL_NAME;
use deepstrike_core::types::message::{Content, ToolCall, ToolResult, ToolSchema};
use futures::stream::FuturesUnordered;
use futures::stream::{self, Stream, StreamExt};

use crate::runtime::sandboxed_skill::{PythonSkillPolicy, SkillKind, execute_json_skill, execute_python_skill, resolve_skill_path};
use crate::Result;
use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::DreamStore;
use crate::run_event::RunEvent;
use crate::tools::{RegisteredTool, ToolChunk, ToolStep, validate_tool_arguments};

#[derive(Clone)]
pub struct ToolSuspendRequest {
    pub call_id: String,
    pub name: String,
    pub suspension_id: String,
    pub payload: Option<serde_json::Value>,
}

pub type ToolSuspendHandler = std::sync::Arc<
    dyn Fn(ToolSuspendRequest) -> futures::future::BoxFuture<'static, Result<serde_json::Value>>
        + Send
        + Sync,
>;

/// Per-run context passed into `ExecutionPlane::execute_all`.
pub struct RunContext<'a> {
    pub agent_id: Option<&'a str>,
    pub skill_dir: Option<&'a Path>,
    pub dream_store: Option<&'a dyn DreamStore>,
    pub knowledge_source: Option<&'a dyn KnowledgeSource>,
    pub governance: Option<Arc<Mutex<Governance>>>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
}

fn make_result(call_id: compact_str::CompactString, output: String, is_error: bool) -> ToolResult {
    ToolResult {
        call_id,
        output: Content::Text(output),
        is_error,
        is_fatal: false,
        token_count: None,
    }
}

/// Guarantees exactly one `tool_result` event per dispatched `ToolCall`.
pub trait ExecutionPlane: Send + Sync {
    fn schemas(&self) -> Vec<ToolSchema>;

    /// Execute a batch of calls. Yields intermediate events; ends with one `ToolResult` per call.
    fn execute_all<'a>(
        &'a self,
        calls: &'a [ToolCall],
        ctx: RunContext<'a>,
    ) -> Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send + 'a>>;
}

/// Executes tools in-process from a registry of `RegisteredTool`s.
pub struct LocalExecutionPlane {
    tools: HashMap<String, Arc<RegisteredTool>>,
}

impl LocalExecutionPlane {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: RegisteredTool) -> &mut Self {
        self.tools
            .insert(tool.schema.name.to_string(), Arc::new(tool));
        self
    }

    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.tools.remove(name);
        self
    }
}

impl Default for LocalExecutionPlane {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionPlane for LocalExecutionPlane {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.values().map(|t| t.schema.clone()).collect()
    }

    fn execute_all<'a>(
        &'a self,
        calls: &'a [ToolCall],
        ctx: RunContext<'a>,
    ) -> Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send + 'a>> {
        Box::pin(execute_all_local(self, calls, ctx))
    }
}

fn execute_all_local<'a>(
    plane: &'a LocalExecutionPlane,
    calls: &'a [ToolCall],
    ctx: RunContext<'a>,
) -> Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send + 'a>> {
    Box::pin(try_stream! {
        let mut permitted = Vec::new();
        for c in calls {
            if let Some(gov) = &ctx.governance {
                let mut g = gov.lock().await;
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                g.set_time(now_ms);
                let args_str =
                    serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".to_string());
                let verdict = g.evaluate(c.name.as_str(), &args_str);
                match verdict.kind.as_str() {
                    "deny" => {
                        let reason = verdict.reason.unwrap_or_default();
                        yield RunEvent::ToolDenied {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            reason: reason.clone(),
                        };
                        yield RunEvent::ToolResult {
                            call_id: c.id.to_string(),
                            content: format!("permission denied: {reason}"),
                            is_error: true,
                        };
                        continue;
                    }
                    "rate_limited" => {
                        let reason = "rate limited".to_string();
                        yield RunEvent::ToolDenied {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            reason: reason.clone(),
                        };
                        yield RunEvent::ToolResult {
                            call_id: c.id.to_string(),
                            content: reason,
                            is_error: true,
                        };
                        continue;
                    }
                    "ask_user" => {
                        let reason = verdict.reason.unwrap_or_default();
                        yield RunEvent::ToolDenied {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            reason: format!("awaiting user approval: {reason}"),
                        };
                        yield RunEvent::ToolResult {
                            call_id: c.id.to_string(),
                            content: "awaiting user approval".into(),
                            is_error: true,
                        };
                        continue;
                    }
                    _ => {}
                }
            }
            permitted.push(c.clone());
        }

        let skill_calls: Vec<_> = permitted
            .iter()
            .filter(|c| c.name.as_str() == SKILL_TOOL_NAME)
            .cloned()
            .collect();
        let memory_calls: Vec<_> = permitted
            .iter()
            .filter(|c| c.name.as_str() == MEMORY_TOOL_NAME)
            .cloned()
            .collect();
        let knowledge_calls: Vec<_> = permitted
            .iter()
            .filter(|c| c.name.as_str() == KNOWLEDGE_TOOL_NAME)
            .cloned()
            .collect();
        let regular_calls: Vec<_> = permitted
            .iter()
            .filter(|c| {
                !matches!(
                    c.name.as_str(),
                    SKILL_TOOL_NAME | MEMORY_TOOL_NAME | KNOWLEDGE_TOOL_NAME
                )
            })
            .cloned()
            .collect();

        for c in skill_calls {
            let name = c.arguments.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let args: std::collections::HashMap<String, serde_json::Value> = c
                .arguments
                .as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();

            let (content, is_error) = if let Some(dir) = ctx.skill_dir {
                match resolve_skill_path(dir, &name) {
                    Some((path, SkillKind::Prompt)) => {
                        match tokio::fs::read_to_string(&path).await {
                            Ok(content) => (strip_frontmatter(&content).to_string(), false),
                            Err(e) => (format!("error reading skill \"{name}\": {e}"), true),
                        }
                    }
                    Some((path, SkillKind::ComputeJson)) => execute_json_skill(&path, &args),
                    Some((path, SkillKind::PythonScript)) => {
                        execute_python_skill(&path, &args, None, &PythonSkillPolicy::default()).await
                    }
                    None => (format!("Skill \"{name}\" not found."), true),
                }
            } else {
                ("No skill directory configured.".into(), true)
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
            };
        }

        for c in memory_calls {
            let query = c.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let top_k = c.arguments.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let (content, is_error) = match (ctx.dream_store, ctx.agent_id) {
                (Some(store), Some(agent_id)) => match store.search(agent_id, &query, top_k).await {
                    Ok(entries) if !entries.is_empty() => {
                        let text = entries
                            .iter()
                            .map(|e| format!("[score={:.3}] {}", e.score, e.text))
                            .collect::<Vec<_>>()
                            .join("\n---\n");
                        (text, false)
                    }
                    Ok(_) => ("No relevant memories found.".into(), false),
                    Err(e) => (format!("Memory search error: {e}"), true),
                },
                _ => ("Memory retrieval not configured.".into(), true),
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
            };
        }

        for c in knowledge_calls {
            let query = c.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let top_k = c.arguments.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let (content, is_error) = if let Some(ks) = ctx.knowledge_source {
                match ks.retrieve(&query, top_k).await {
                    Ok(snippets) if !snippets.is_empty() => (snippets.join("\n---\n"), false),
                    Ok(_) => ("No relevant knowledge found.".into(), false),
                    Err(e) => (format!("Knowledge retrieval error: {e}"), true),
                }
            } else {
                ("Knowledge source not configured.".into(), true)
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
            };
        }

        if regular_calls.is_empty() {
            return;
        }

        struct ActiveTool {
            call_id: compact_str::CompactString,
            name: String,
            session: Box<dyn crate::tools::ToolSession>,
            resume_input: Option<serde_json::Value>,
            combined: String,
        }

        let mut active: FuturesUnordered<
            futures::future::BoxFuture<'_, (ActiveTool, crate::Result<ToolStep>)>,
        > = FuturesUnordered::new();

        for mut call in regular_calls {
            let Some(tool) = plane.tools.get(call.name.as_str()) else {
                let content = format!("unknown tool: {}", call.name);
                yield RunEvent::ToolResult {
                    call_id: call.id.to_string(),
                    content: content.clone(),
                    is_error: true,
                };
                continue;
            };
            let original_args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
            match validate_tool_arguments(&tool.schema.parameters, &mut call.arguments) {
                Ok(repaired) => {
                    if repaired {
                        let repaired_args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
                        yield RunEvent::ToolArgumentRepaired {
                            call_id: call.id.to_string(),
                            name: call.name.to_string(),
                            original_arguments: original_args_str,
                            repaired_arguments: repaired_args_str,
                        };
                    }
                }
                Err(e) => {
                    let content = format!("invalid arguments: {e}");
                    yield RunEvent::ToolResult {
                        call_id: call.id.to_string(),
                        content: content.clone(),
                        is_error: true,
                    };
                    continue;
                }
            }
            let start = Arc::clone(&tool.start);
            let args = call.arguments.clone();
            let call_id = call.id.clone();
            let name = call.name.to_string();
            match start(args).await {
                Ok(session) => {
                    let active_tool = ActiveTool {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        session,
                        resume_input: None,
                        combined: String::new(),
                    };
                    active.push(Box::pin(async move {
                        let mut t = active_tool;
                        let step = t.session.next(t.resume_input.take()).await;
                        (t, step)
                    }));
                }
                Err(e) => {
                    yield RunEvent::ToolResult {
                        call_id: call_id.to_string(),
                        content: e.to_string(),
                        is_error: true,
                    };
                }
            }
        }

        while let Some((mut tool, step)) = active.next().await {
            match step {
                Ok(ToolStep::Chunk(chunk)) => {
                    match &chunk {
                        ToolChunk::Suspend { suspension_id, payload } => {
                            yield RunEvent::ToolSuspend {
                                call_id: tool.call_id.to_string(),
                                name: tool.name.clone(),
                                suspension_id: suspension_id.clone(),
                                payload: payload.clone(),
                            };
                            match &ctx.on_tool_suspend {
                                Some(handler) => {
                                    tool.resume_input = Some(
                                        handler(ToolSuspendRequest {
                                            call_id: tool.call_id.to_string(),
                                            name: tool.name.clone(),
                                            suspension_id: suspension_id.clone(),
                                            payload: payload.clone(),
                                        })
                                        .await?,
                                    );
                                }
                                None => {
                                    let content = format!(
                                        "tool suspended without resume handler: {suspension_id}"
                                    );
                                    yield RunEvent::ToolResult {
                                        call_id: tool.call_id.to_string(),
                                        content: content.clone(),
                                        is_error: true,
                                    };
                                    continue;
                                }
                            }
                        }
                        _ => {
                            tool.combined.push_str(chunk.text_projection());
                            yield RunEvent::ToolDelta {
                                call_id: tool.call_id.to_string(),
                                name: tool.name.clone(),
                                chunk,
                            };
                        }
                    }
                    active.push(Box::pin(async move {
                        let step = tool.session.next(tool.resume_input.take()).await;
                        (tool, step)
                    }));
                }
                Ok(ToolStep::Done(text)) => {
                    tool.combined.push_str(&text);
                    yield RunEvent::ToolResult {
                        call_id: tool.call_id.to_string(),
                        content: tool.combined.clone(),
                        is_error: false,
                    };
                }
                Err(e) => {
                    yield RunEvent::ToolResult {
                        call_id: tool.call_id.to_string(),
                        content: e.to_string(),
                        is_error: true,
                    };
                }
            }
        }
    })
}

fn strip_frontmatter(content: &str) -> &str {
    let s = content.trim_start();
    if !s.starts_with("---") {
        return s;
    }
    let rest = &s[3..];
    if let Some(end) = rest.find("\n---") {
        rest[end + 4..].trim_start_matches('\n')
    } else {
        s
    }
}

/// Collect tool results from a plane stream (one result per initial call).
pub async fn collect_tool_results(
    mut stream: Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send>>,
    calls: &[ToolCall],
) -> Result<Vec<ToolResult>> {
    let mut by_id: HashMap<String, ToolResult> = HashMap::new();
    while let Some(evt) = stream.next().await {
        if let RunEvent::ToolResult {
            call_id,
            content,
            is_error,
        } = evt?
        {
            by_id.insert(
                call_id.clone(),
                make_result(compact_str::CompactString::new(&call_id), content, is_error),
            );
        }
    }
    Ok(calls
        .iter()
        .filter_map(|c| by_id.remove(c.id.as_str()))
        .collect())
}
