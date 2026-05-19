use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;

use async_stream::try_stream;
use deepstrike_core::types::message::{ToolCall, ToolSchema};
use futures::stream::Stream;
use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::run_event::RunEvent;
use crate::runtime::credential_vault::CredentialVault;
use crate::runtime::execution_plane::{ExecutionPlane, LocalExecutionPlane, RunContext};
use crate::tools::RegisteredTool;
use crate::{Error, Result};

// ── Server config ─────────────────────────────────────────────────────────────

pub struct McpServerConfig {
    /// Executable to run (e.g. "npx", "python3", "/usr/local/bin/my-mcp-server").
    pub command: String,
    pub args: Vec<String>,
    /// Vault keys injected as env vars into the subprocess. Never exposed to the model.
    pub credential_keys: Vec<String>,
    /// Additional static env vars forwarded to the subprocess.
    pub env: HashMap<String, String>,
}

// ── Internal MCP connection ───────────────────────────────────────────────────

struct Inner {
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

struct McpConnection {
    server_name: String,
    inner: Mutex<Inner>,
    child: Mutex<Option<tokio::process::Child>>,
    schemas: Vec<ToolSchema>,
}

async fn do_request(
    inner: &mut Inner,
    server_name: &str,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    let id = inner.next_id;
    inner.next_id += 1;

    let mut msg = serde_json::json!({ "jsonrpc": "2.0", "method": method, "id": id });
    if let Some(p) = params {
        msg["params"] = p;
    }
    let line = serde_json::to_string(&msg).map_err(|e| Error::Other(e.to_string()))? + "\n";

    inner.stdin.write_all(line.as_bytes()).await.map_err(|e| Error::Other(e.to_string()))?;
    inner.stdin.flush().await.map_err(|e| Error::Other(e.to_string()))?;

    loop {
        let mut buf = String::new();
        let n = inner.reader.read_line(&mut buf).await.map_err(|e| Error::Other(e.to_string()))?;
        if n == 0 {
            return Err(Error::Other(format!("MCP server '{server_name}' disconnected")));
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(resp) = serde_json::from_str::<Value>(trimmed) else { continue };
        // Skip notifications (no numeric id)
        let Some(resp_id) = resp.get("id").and_then(Value::as_u64) else { continue };
        if resp_id != id {
            continue;
        }
        if let Some(err) = resp.get("error") {
            return Err(Error::Other(format!("MCP({server_name}) error: {err}")));
        }
        return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
    }
}

async fn do_notify(inner: &mut Inner, method: &str) {
    let msg = serde_json::json!({ "jsonrpc": "2.0", "method": method });
    if let Ok(s) = serde_json::to_string(&msg) {
        inner.stdin.write_all((s + "\n").as_bytes()).await.ok();
        inner.stdin.flush().await.ok();
    }
}

impl McpConnection {
    async fn start(
        server_name: &str,
        config: &McpServerConfig,
        vault: &dyn CredentialVault,
    ) -> Result<Arc<Self>> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        for (k, v) in &config.env {
            env.insert(k.clone(), v.clone());
        }
        for key in &config.credential_keys {
            if let Some(val) = vault.get(key).await {
                env.insert(key.clone(), val);
            }
        }

        let mut child = tokio::process::Command::new(&config.command)
            .args(&config.args)
            .envs(&env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Other(format!("failed to spawn MCP server '{server_name}': {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Other(format!("MCP server '{server_name}': no stdin handle")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other(format!("MCP server '{server_name}': no stdout handle")))?;

        let mut inner = Inner { stdin, reader: BufReader::new(stdout), next_id: 1 };

        // MCP handshake
        do_request(
            &mut inner,
            server_name,
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "clientInfo": { "name": "deepstrike", "version": "0.1.0" },
            })),
        )
        .await?;
        do_notify(&mut inner, "notifications/initialized").await;

        // Discover tool schemas
        let list = do_request(&mut inner, server_name, "tools/list", None).await?;
        let raw_tools = list.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();

        let mut schemas = Vec::new();
        for t in &raw_tools {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let description = t.get("description").and_then(Value::as_str).unwrap_or(name);
            let parameters = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));
            schemas.push(ToolSchema {
                name: name.into(),
                description: description.to_string(),
                parameters,
            });
        }

        Ok(Arc::new(McpConnection {
            server_name: server_name.to_string(),
            inner: Mutex::new(inner),
            child: Mutex::new(Some(child)),
            schemas,
        }))
    }

    async fn execute(&self, call: &ToolCall) -> (String, bool) {
        let params = serde_json::json!({
            "name": call.name.as_str(),
            "arguments": call.arguments,
        });
        let mut inner = self.inner.lock().await;
        match do_request(&mut inner, &self.server_name, "tools/call", Some(params)).await {
            Ok(result) => {
                let text = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                            .filter_map(|c| c.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let is_error =
                    result.get("isError").and_then(Value::as_bool).unwrap_or(false);
                let output = if text.is_empty() {
                    serde_json::to_string(&result).unwrap_or_default()
                } else {
                    text
                };
                (output, is_error)
            }
            Err(e) => (e.to_string(), true),
        }
    }

    async fn stop(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            child.kill().await.ok();
            child.wait().await.ok();
        }
    }
}

// ── Public plane ──────────────────────────────────────────────────────────────

/// `ExecutionPlane` that proxies tool calls to MCP servers over JSON-RPC 2.0 (stdio).
///
/// Credentials live in a `CredentialVault` and are injected into each server's subprocess
/// environment — the model never sees the credential values.
///
/// Usage:
/// ```ignore
/// let mut plane = McpProxyPlane::new(Arc::new(EnvCredentialVault));
/// plane.add_server("brave", McpServerConfig {
///     command: "npx".into(),
///     args: vec!["-y".into(), "@modelcontextprotocol/server-brave-search".into()],
///     credential_keys: vec!["BRAVE_API_KEY".into()],
///     env: Default::default(),
/// });
/// plane.connect().await?;
/// // ... use with RuntimeRunner ...
/// plane.disconnect().await;
/// ```
pub struct McpProxyPlane {
    server_configs: HashMap<String, McpServerConfig>,
    vault: Arc<dyn CredentialVault>,
    connections: Vec<Arc<McpConnection>>,
    tool_to_conn: HashMap<String, Arc<McpConnection>>,
    local: LocalExecutionPlane,
    local_names: HashSet<String>,
}

impl McpProxyPlane {
    pub fn new(vault: Arc<dyn CredentialVault>) -> Self {
        Self {
            server_configs: HashMap::new(),
            vault,
            connections: Vec::new(),
            tool_to_conn: HashMap::new(),
            local: LocalExecutionPlane::new(),
            local_names: HashSet::new(),
        }
    }

    pub fn add_server(&mut self, name: impl Into<String>, config: McpServerConfig) -> &mut Self {
        self.server_configs.insert(name.into(), config);
        self
    }

    /// Start all configured MCP server processes and discover their tool schemas.
    pub async fn connect(&mut self) -> Result<()> {
        for (name, config) in &self.server_configs {
            let conn = McpConnection::start(name, config, self.vault.as_ref()).await?;
            for schema in &conn.schemas {
                self.tool_to_conn.insert(schema.name.to_string(), Arc::clone(&conn));
            }
            self.connections.push(conn);
        }
        Ok(())
    }

    /// Gracefully stop all MCP server processes.
    pub async fn disconnect(&mut self) {
        for conn in &self.connections {
            conn.stop().await;
        }
        self.connections.clear();
        self.tool_to_conn.clear();
    }

    /// Register an in-process tool. Local tools take priority over MCP tools of the same name.
    pub fn register(&mut self, tool: RegisteredTool) -> &mut Self {
        self.local_names.insert(tool.schema.name.to_string());
        self.local.register(tool);
        self
    }

    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.local_names.remove(name);
        self.local.unregister(name);
        self
    }
}

impl ExecutionPlane for McpProxyPlane {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut result = self.local.schemas();
        for conn in &self.connections {
            result.extend(conn.schemas.iter().cloned());
        }
        result
    }

    fn execute_all<'a>(
        &'a self,
        calls: &'a [ToolCall],
        ctx: RunContext<'a>,
    ) -> Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send + 'a>> {
        Box::pin(try_stream! {
            let RunContext {
                agent_id, skill_dir, dream_store, knowledge_source, governance, on_tool_suspend,
            } = ctx;

            let local_calls: Vec<_> = calls
                .iter()
                .filter(|c| self.local_names.contains(c.name.as_str()))
                .cloned()
                .collect();
            let mcp_calls: Vec<_> = calls
                .iter()
                .filter(|c| !self.local_names.contains(c.name.as_str()))
                .cloned()
                .collect();

            if !local_calls.is_empty() {
                let local_ctx = RunContext {
                    agent_id,
                    skill_dir,
                    dream_store,
                    knowledge_source,
                    governance: governance.clone(),
                    on_tool_suspend: on_tool_suspend.clone(),
                };
                let mut s = self.local.execute_all(&local_calls, local_ctx);
                while let Some(evt) = s.next().await {
                    yield evt?;
                }
            }

            // Group MCP calls by connection; concurrent across servers, sequential within.
            let mut groups: HashMap<usize, (Arc<McpConnection>, Vec<ToolCall>)> = HashMap::new();
            let mut unknown: Vec<ToolCall> = Vec::new();
            for call in &mcp_calls {
                if let Some(conn) = self.tool_to_conn.get(call.name.as_str()) {
                    let key = Arc::as_ptr(conn) as usize;
                    let entry = groups.entry(key).or_insert_with(|| (Arc::clone(conn), Vec::new()));
                    entry.1.push(call.clone());
                } else {
                    unknown.push(call.clone());
                }
            }

            for call in &unknown {
                yield RunEvent::ToolResult {
                    call_id: call.id.to_string(),
                    content: format!("unknown MCP tool: {}", call.name),
                    is_error: true,
                };
            }

            let futs: Vec<_> = groups
                .into_values()
                .map(|(conn, group_calls)| async move {
                    let mut results = Vec::new();
                    for call in &group_calls {
                        let (output, is_error) = conn.execute(call).await;
                        results.push((call.id.to_string(), output, is_error));
                    }
                    results
                })
                .collect();

            let all_results = futures::future::join_all(futs).await;
            for group in all_results {
                for (call_id, content, is_error) in group {
                    yield RunEvent::ToolResult { call_id, content, is_error };
                }
            }
        })
    }
}
