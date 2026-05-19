use std::collections::HashSet;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::try_stream;
use deepstrike_core::types::message::{ToolCall, ToolSchema};
use futures::stream::Stream;
use futures::StreamExt;

use crate::run_event::RunEvent;
use crate::runtime::credential_vault::CredentialVault;
use crate::runtime::execution_plane::{ExecutionPlane, LocalExecutionPlane, RunContext};
use crate::tools::RegisteredTool;
use crate::Result;

pub struct RemoteVpcOptions {
    /// Base URL of the remote worker endpoint inside the customer VPC.
    /// Expected route: `POST {base_url}/execute`  body: `{ name, arguments }`
    ///                                            response: `{ output, isError }`
    pub base_url: String,
    pub vault: Arc<dyn CredentialVault>,
    /// Vault key whose value is sent verbatim as the `Authorization` header.
    /// Fetched fresh on every `execute_all` call — never stored in the session log.
    pub auth_credential_key: Option<String>,
    /// Static tool schemas served by this VPC worker.
    pub schemas: Vec<ToolSchema>,
    /// Per-call HTTP timeout in ms. Default: 30 000.
    pub timeout_ms: u64,
}

/// `ExecutionPlane` that forwards tool calls over HTTP to a worker inside a customer VPC.
///
/// Credentials are fetched from a `CredentialVault` at call time and injected into the
/// `Authorization` header — they are never forwarded to the model or stored in the session log.
///
/// Local tools registered via `register()` run in-process and take priority over any remote
/// schema with the same name.
pub struct RemoteVpcPlane {
    base_url: String,
    vault: Arc<dyn CredentialVault>,
    auth_key: Option<String>,
    remote_schemas: Vec<ToolSchema>,
    client: reqwest::Client,
    local: LocalExecutionPlane,
}

impl RemoteVpcPlane {
    pub fn new(opts: RemoteVpcOptions) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(opts.timeout_ms))
            .build()
            .unwrap_or_default();
        Self {
            base_url: opts.base_url.trim_end_matches('/').to_string(),
            vault: opts.vault,
            auth_key: opts.auth_credential_key,
            remote_schemas: opts.schemas,
            client,
            local: LocalExecutionPlane::new(),
        }
    }

    /// Register an in-process tool. Local tools take priority over remote schemas of the same name.
    pub fn register(&mut self, tool: RegisteredTool) -> &mut Self {
        self.local.register(tool);
        self
    }

    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.local.unregister(name);
        self
    }
}

impl ExecutionPlane for RemoteVpcPlane {
    fn schemas(&self) -> Vec<ToolSchema> {
        let local_names: HashSet<_> =
            self.local.schemas().into_iter().map(|s| s.name.clone()).collect();
        let mut result = self.local.schemas();
        result.extend(
            self.remote_schemas.iter().filter(|s| !local_names.contains(&s.name)).cloned(),
        );
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

            let local_names: HashSet<String> =
                self.local.schemas().into_iter().map(|s| s.name.to_string()).collect();
            let local_calls: Vec<_> =
                calls.iter().filter(|c| local_names.contains(c.name.as_str())).cloned().collect();
            let remote_calls: Vec<_> =
                calls.iter().filter(|c| !local_names.contains(c.name.as_str())).cloned().collect();

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

            if !remote_calls.is_empty() {
                let auth = if let Some(key) = &self.auth_key {
                    self.vault.get(key).await
                } else {
                    None
                };

                // Fire all remote calls concurrently; yield results in dispatch order.
                let futs: Vec<_> = remote_calls
                    .iter()
                    .map(|call| {
                        let client = self.client.clone();
                        let url = format!("{}/execute", self.base_url);
                        let auth = auth.clone();
                        let call = call.clone();
                        async move { call_remote(client, url, auth, call).await }
                    })
                    .collect();

                let results = futures::future::join_all(futs).await;
                for (call, (content, is_error)) in remote_calls.iter().zip(results) {
                    yield RunEvent::ToolResult {
                        call_id: call.id.to_string(),
                        content,
                        is_error,
                    };
                }
            }
        })
    }
}

// ── HTTP helper ───────────────────────────────────────────────────────────────

async fn call_remote(
    client: reqwest::Client,
    url: String,
    auth: Option<String>,
    call: ToolCall,
) -> (String, bool) {
    let mut req = client.post(&url).json(&serde_json::json!({
        "name": call.name.as_str(),
        "arguments": call.arguments,
    }));
    if let Some(token) = auth {
        req = req.header("Authorization", token);
    }

    match req.send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                let msg = if body.is_empty() {
                    format!("HTTP {status}")
                } else {
                    format!("HTTP {status}: {body}")
                };
                return (msg, true);
            }
            match resp.json::<serde_json::Value>().await {
                Ok(result) => {
                    let output =
                        result.get("output").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let is_error =
                        result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                    (output, is_error)
                }
                Err(e) => (e.to_string(), true),
            }
        }
        Err(e) => (e.to_string(), true),
    }
}
