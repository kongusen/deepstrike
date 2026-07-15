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

use crate::governance::Governance;
use crate::knowledge::KnowledgeSource;
use crate::memory::DreamStore;
use deepstrike_core::mm::memory::{MemoryQuery, MemoryScope};
use crate::run_event::RunEvent;
use crate::runtime::sandboxed_skill::{
    execute_json_skill, execute_python_skill, resolve_skill_path, PythonSkillPolicy, SkillKind,
};
use crate::tools::{validate_tool_arguments, RegisteredTool, ToolChunk, ToolStep};
use crate::Result;

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

#[derive(Clone)]
pub struct PermissionRequest {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
    pub reason: String,
}

#[derive(Clone)]
pub struct PermissionResponse {
    pub approved: bool,
    pub responder: String,
    pub reason: Option<String>,
}

pub type PermissionRequestHandler = std::sync::Arc<
    dyn Fn(PermissionRequest) -> futures::future::BoxFuture<'static, Result<PermissionResponse>>
        + Send
        + Sync,
>;

/// Per-run context passed into `ExecutionPlane::execute_all`.
pub struct RunContext<'a> {
    pub agent_id: Option<&'a str>,
    pub memory_scope: Option<&'a MemoryScope>,
    pub skill_dir: Option<&'a Path>,
    pub dream_store: Option<&'a dyn DreamStore>,
    pub knowledge_source: Option<&'a dyn KnowledgeSource>,
    pub governance: Option<Arc<Mutex<Governance>>>,
    pub on_tool_suspend: Option<ToolSuspendHandler>,
    pub on_permission_request: Option<PermissionRequestHandler>,
}

fn make_result(
    call_id: compact_str::CompactString,
    output: String,
    is_error: bool,
    is_fatal: bool,
    error_kind: Option<deepstrike_core::types::message::ToolErrorKind>,
) -> ToolResult {
    ToolResult {
        call_id,
        output: Content::Text(output),
        is_error,
        is_fatal,
        error_kind,
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
                            is_fatal: false,
                            error_kind: Some(deepstrike_core::types::message::ToolErrorKind::GovernanceDenied),
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
                            is_fatal: false,
                            error_kind: Some(deepstrike_core::types::message::ToolErrorKind::Recoverable),
                        };
                        continue;
                    }
                    "ask_user" => {
                        let reason = verdict.reason.unwrap_or_default();
                        let args_str = serde_json::to_string(&c.arguments)
                            .unwrap_or_else(|_| "{}".to_string());
                        let request = PermissionRequest {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            arguments: args_str.clone(),
                            reason: reason.clone(),
                        };
                        yield RunEvent::PermissionRequest {
                            call_id: request.call_id.clone(),
                            tool_name: request.tool_name.clone(),
                            arguments: args_str,
                            reason: reason.clone(),
                        };

                        let decision = resolve_permission_request(request, &ctx).await;
                        yield RunEvent::PermissionResolved {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            approved: decision.approved,
                            responder: decision.responder.clone(),
                            reason: decision.reason.clone(),
                        };
                        if decision.approved {
                            permitted.push(c.clone());
                            continue;
                        }

                        let denied_reason = decision.reason.unwrap_or_else(|| {
                            if reason.is_empty() { "permission denied".to_string() } else { reason.clone() }
                        });
                        yield RunEvent::ToolDenied {
                            call_id: c.id.to_string(),
                            tool_name: c.name.to_string(),
                            reason: denied_reason.clone(),
                        };
                        yield RunEvent::ToolResult {
                            call_id: c.id.to_string(),
                            content: format!("permission denied: {denied_reason}"),
                            is_error: true,
                            is_fatal: false,
                            error_kind: Some(deepstrike_core::types::message::ToolErrorKind::GovernanceDenied),
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
            let error_kind = if is_error {
                Some(deepstrike_core::types::message::ToolErrorKind::Recoverable)
            } else {
                None
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
                is_fatal: false,
                error_kind,
            };
        }

        for c in memory_calls {
            let query = c.arguments.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let top_k = c.arguments.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let (content, is_error) = match (ctx.dream_store, ctx.agent_id, ctx.memory_scope) {
                (Some(store), Some(agent_id), Some(scope)) => match store.search(agent_id, &MemoryQuery {
                    scope: scope.clone(),
                    query,
                    top_k,
                    kinds: Vec::new(),
                    min_score: None,
                }).await {
                    Ok(entries) if !entries.is_empty() => {
                        let text = entries
                            .iter()
                            .map(|e| format!(
                                "[memory record_id={} score={:.3}] {}",
                                e.record.record_id, e.score, e.record.content
                            ))
                            .collect::<Vec<_>>()
                            .join("\n---\n");
                        (text, false)
                    }
                    Ok(_) => ("No relevant memories found.".into(), false),
                    Err(e) => (format!("Memory search error: {e}"), true),
                },
                _ => ("Memory retrieval not configured.".into(), true),
            };
            let error_kind = if is_error {
                Some(deepstrike_core::types::message::ToolErrorKind::Recoverable)
            } else {
                None
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
                is_fatal: false,
                error_kind,
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
            let error_kind = if is_error {
                Some(deepstrike_core::types::message::ToolErrorKind::Recoverable)
            } else {
                None
            };
            yield RunEvent::ToolResult {
                call_id: c.id.to_string(),
                content,
                is_error,
                is_fatal: false,
                error_kind,
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
            if let Some(content) = try_read_spooled_argument(&call).await {
                yield RunEvent::ToolResult {
                    call_id: call.id.to_string(),
                    content,
                    is_error: false,
                    is_fatal: false,
                    error_kind: None,
                };
                continue;
            }

            let Some(tool) = plane.tools.get(call.name.as_str()) else {
                let content = format!("unknown tool: {}", call.name);
                yield RunEvent::ToolResult {
                    call_id: call.id.to_string(),
                    content: content.clone(),
                    is_error: true,
                    is_fatal: false,
                    error_kind: Some(deepstrike_core::types::message::ToolErrorKind::Recoverable),
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
                        is_fatal: false,
                        error_kind: Some(deepstrike_core::types::message::ToolErrorKind::Recoverable),
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
                    let (is_fatal, error_kind) = match &e {
                        crate::Error::ToolExecutionFailed { is_fatal, error_kind, .. } => (*is_fatal, *error_kind),
                        crate::Error::ToolFail { is_fatal, error_kind, .. } => (*is_fatal, *error_kind),
                        _ => (false, Some(deepstrike_core::types::message::ToolErrorKind::Recoverable)),
                    };
                    yield RunEvent::ToolResult {
                        call_id: call_id.to_string(),
                        content: crate::format_tool_error(&e),
                        is_error: true,
                        is_fatal,
                        error_kind,
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
                                        is_fatal: false,
                                        error_kind: Some(deepstrike_core::types::message::ToolErrorKind::Recoverable),
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
                        is_fatal: false,
                        error_kind: None,
                    };
                }
                Err(e) => {
                    let (is_fatal, error_kind) = match &e {
                        crate::Error::ToolExecutionFailed { is_fatal, error_kind, .. } => (*is_fatal, *error_kind),
                        crate::Error::ToolFail { is_fatal, error_kind, .. } => (*is_fatal, *error_kind),
                        _ => (false, Some(deepstrike_core::types::message::ToolErrorKind::Recoverable)),
                    };
                    yield RunEvent::ToolResult {
                        call_id: tool.call_id.to_string(),
                        content: crate::format_tool_error(&e),
                        is_error: true,
                        is_fatal,
                        error_kind,
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

async fn resolve_permission_request(
    request: PermissionRequest,
    ctx: &RunContext<'_>,
) -> PermissionResponse {
    let Some(handler) = &ctx.on_permission_request else {
        return PermissionResponse {
            approved: false,
            responder: "policy_gate".to_string(),
            reason: Some("no permission handler configured".to_string()),
        };
    };

    match handler(request).await {
        Ok(response) => PermissionResponse {
            approved: response.approved,
            responder: if response.responder.is_empty() {
                "host".to_string()
            } else {
                response.responder
            },
            reason: response.reason,
        },
        Err(err) => PermissionResponse {
            approved: false,
            responder: "permission_handler".to_string(),
            reason: Some(format!("permission handler failed: {err}")),
        },
    }
}

async fn try_read_spooled_argument(call: &ToolCall) -> Option<String> {
    let is_read_tool = matches!(
        call.name.as_str(),
        "read" | "read_file" | "view_file" | "read_spooled_result"
    );
    if !is_read_tool {
        return None;
    }
    let obj = call.arguments.as_object()?;
    for val in obj.values() {
        if let Some(s) = val.as_str() {
            if s.starts_with(".spool/") || s.contains("/.spool/") {
                if let Ok(content) = tokio::fs::read_to_string(s).await {
                    return Some(content);
                }
            }
        }
    }
    None
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
            is_fatal,
            error_kind,
        } = evt?
        {
            by_id.insert(
                call_id.clone(),
                make_result(
                    compact_str::CompactString::new(&call_id),
                    content,
                    is_error,
                    is_fatal,
                    error_kind,
                ),
            );
        }
    }
    Ok(calls
        .iter()
        .filter_map(|c| by_id.remove(c.id.as_str()))
        .collect())
}
