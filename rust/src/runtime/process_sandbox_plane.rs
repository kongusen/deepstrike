use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use deepstrike_core::types::message::{ToolCall, ToolSchema};
use futures::stream::Stream;
use serde_json::json;
use tokio::io::AsyncReadExt;

use crate::Result;
use crate::run_event::RunEvent;
use crate::runtime::execution_plane::{ExecutionPlane, LocalExecutionPlane, RunContext};
use crate::tools::RegisteredTool;

pub struct SandboxOptions {
    /// Working directory for all subprocesses; isolated from the host file system by convention.
    pub sandbox_dir: PathBuf,
    /// Host env-var names forwarded into subprocesses. Default: none.
    pub allowed_env_keys: Vec<String>,
    /// Per-call hard timeout in ms. Default: 30 000.
    pub timeout_ms: u64,
    /// Truncate stdout+stderr after this many bytes. Default: 1 MiB.
    pub max_output_bytes: usize,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            sandbox_dir: std::env::temp_dir().join("deepstrike-sandbox"),
            allowed_env_keys: vec![],
            timeout_ms: 30_000,
            max_output_bytes: 1_048_576,
        }
    }
}

/// `LocalExecutionPlane` extended with two subprocess built-in tools:
///   - `run_bash`   — executes a bash command inside `sandbox_dir`.
///   - `run_python` — evaluates a Python script inside `sandbox_dir`.
///
/// All registered Rust tools still run in-process (identical to `LocalExecutionPlane`).
/// Subprocesses are launched with `sandbox_dir` as cwd and a stripped environment.
/// This is an execution hygiene boundary, not an OS-enforced filesystem sandbox.
pub struct ProcessSandboxPlane {
    inner: LocalExecutionPlane,
}

impl ProcessSandboxPlane {
    pub fn new(opts: SandboxOptions) -> Self {
        let opts = Arc::new(opts);
        let mut plane = LocalExecutionPlane::new();
        plane.register(make_bash_tool(Arc::clone(&opts)));
        plane.register(make_python_tool(opts));
        Self { inner: plane }
    }

    pub fn register(&mut self, tool: RegisteredTool) -> &mut Self {
        self.inner.register(tool);
        self
    }

    pub fn unregister(&mut self, name: &str) -> &mut Self {
        self.inner.unregister(name);
        self
    }
}

impl ExecutionPlane for ProcessSandboxPlane {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner.schemas()
    }

    fn execute_all<'a>(
        &'a self,
        calls: &'a [ToolCall],
        ctx: RunContext<'a>,
    ) -> Pin<Box<dyn Stream<Item = Result<RunEvent>> + Send + 'a>> {
        self.inner.execute_all(calls, ctx)
    }
}

// ── Subprocess runner ─────────────────────────────────────────────────────────

async fn run_subprocess(
    cmd: &'static str,
    script: String,
    sandbox_dir: PathBuf,
    allowed_env_keys: Vec<String>,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> (String, bool) {
    if let Err(e) = tokio::fs::create_dir_all(&sandbox_dir).await {
        return (e.to_string(), true);
    }

    let mut env: HashMap<String, String> = HashMap::new();
    let dir = sandbox_dir.to_string_lossy().to_string();
    env.insert("HOME".into(), dir.clone());
    env.insert("TMPDIR".into(), dir);
    env.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
    for key in &allowed_env_keys {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.clone(), val);
        }
    }

    let mut child = match tokio::process::Command::new(cmd)
        .arg("-c")
        .arg(&script)
        .current_dir(&sandbox_dir)
        .envs(&env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (e.to_string(), true),
    };

    // Drain stdout/stderr concurrently so the child never blocks on a full pipe buffer.
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let read_out = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::BufReader::new(stdout_pipe)
            .read_to_end(&mut buf)
            .await
            .ok();
        buf
    });
    let read_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::BufReader::new(stderr_pipe)
            .read_to_end(&mut buf)
            .await
            .ok();
        buf
    });

    // wait() takes &mut self, so child remains usable if the timeout fires.
    let timed_out =
        tokio::time::timeout(tokio::time::Duration::from_millis(timeout_ms), child.wait()).await;

    let is_error = match timed_out {
        Ok(Ok(status)) => !status.success(),
        Ok(Err(e)) => {
            read_out.abort();
            read_err.abort();
            return (e.to_string(), true);
        }
        Err(_) => {
            child.kill().await.ok();
            child.wait().await.ok();
            read_out.abort();
            read_err.abort();
            return (format!("timed out after {timeout_ms}ms"), true);
        }
    };

    let out_bytes = read_out.await.unwrap_or_default();
    let err_bytes = read_err.await.unwrap_or_default();
    let mut combined = [out_bytes, err_bytes].concat();
    if combined.len() > max_output_bytes {
        combined.truncate(max_output_bytes);
        combined.extend_from_slice(b"\n[output truncated]");
    }
    let text = String::from_utf8_lossy(&combined).into_owned();
    if is_error && text.trim().is_empty() {
        return (
            "Process exited with non-zero status and produced no output.".into(),
            true,
        );
    }
    (
        if text.is_empty() {
            "(no output)".into()
        } else {
            text
        },
        is_error,
    )
}

// ── Tool constructors ─────────────────────────────────────────────────────────

fn make_bash_tool(opts: Arc<SandboxOptions>) -> RegisteredTool {
    RegisteredTool::text(
        "run_bash",
        "Run a bash command with the sandbox directory as cwd and a stripped environment. \
         This is not an OS-enforced filesystem sandbox.",
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The bash command to execute." }
            },
            "required": ["command"]
        }),
        move |args| {
            let opts = Arc::clone(&opts);
            Box::pin(async move {
                let command = args["command"].as_str().unwrap_or("").to_string();
                let (output, _) = run_subprocess(
                    "bash",
                    command,
                    opts.sandbox_dir.clone(),
                    opts.allowed_env_keys.clone(),
                    opts.timeout_ms,
                    opts.max_output_bytes,
                )
                .await;
                Ok(output)
            })
        },
    )
}

fn make_python_tool(opts: Arc<SandboxOptions>) -> RegisteredTool {
    RegisteredTool::text(
        "run_python",
        "Evaluate a Python script with the sandbox directory as cwd and a stripped environment.",
        json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "The Python code to evaluate." }
            },
            "required": ["code"]
        }),
        move |args| {
            let opts = Arc::clone(&opts);
            Box::pin(async move {
                let code = args["code"].as_str().unwrap_or("").to_string();
                let (output, _) = run_subprocess(
                    "python3",
                    code,
                    opts.sandbox_dir.clone(),
                    opts.allowed_env_keys.clone(),
                    opts.timeout_ms,
                    opts.max_output_bytes,
                )
                .await;
                Ok(output)
            })
        },
    )
}
