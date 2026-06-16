use std::collections::HashMap;
use std::path::Path;

use deepstrike_core::types::skill::SkillMetadata;

/// Execution kind for a skill, determined by file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillKind {
    /// `.md` — injected as context into the LLM (existing behavior).
    Prompt,
    /// `.json` — rendered client-side via `{{key}}` template engine (Phase A).
    ComputeJson,
    /// `.py` — executed via sandboxed Python subprocess (Phase B).
    PythonScript,
}

impl SkillKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("md") => Some(Self::Prompt),
            Some("json") => Some(Self::ComputeJson),
            Some("py") => Some(Self::PythonScript),
            _ => None,
        }
    }
}

/// Resolve the filesystem path for a skill by name, trying `.md`, `.json`,
/// then `.py` in priority order.
pub fn resolve_skill_path(skill_dir: &Path, name: &str) -> Option<(std::path::PathBuf, SkillKind)> {
    for (ext, kind) in &[
        ("md", SkillKind::Prompt),
        ("json", SkillKind::ComputeJson),
        ("py", SkillKind::PythonScript),
    ] {
        let path = skill_dir.join(format!("{name}.{ext}"));
        if path.exists() {
            return Some((path, *kind));
        }
    }
    None
}

// ── Phase A: JSON pure-compute skills ────────────────────────────────────────

/// Parse metadata from a `.json` skill file.
///
/// JSON skill format:
/// ```json
/// {
///   "name": "greet",
///   "description": "Return a greeting string",
///   "when_to_use": "greeting, hello",
///   "template": "Hello, {{name}}! You have {{count}} messages."
/// }
/// ```
pub fn parse_json_skill(path: &Path) -> Option<SkillMetadata> {
    let content = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let name = v["name"].as_str()?.to_string();
    let description = v["description"].as_str().unwrap_or("").to_string();
    let mut meta = SkillMetadata::new(name, description);
    if let Some(w) = v["when_to_use"].as_str() {
        meta = meta.with_when_to_use(w);
    }
    // P1-B: `allowed_tools` JSON array → declared tool ids for skill gating.
    if let Some(tools) = v["allowed_tools"].as_array() {
        meta.allowed_tools = tools.iter().filter_map(|t| t.as_str()).map(Into::into).collect();
    }
    Some(meta)
}

/// Execute a JSON skill by rendering its `template` field with caller-supplied args.
///
/// Variables are substituted using `{{key}}` syntax; unmatched placeholders are
/// left as-is so the LLM can see what was missing.
pub fn execute_json_skill(path: &Path, args: &HashMap<String, serde_json::Value>) -> (String, bool) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("error: could not read skill file: {e}"), true),
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return ("error: invalid JSON skill format".into(), true),
    };
    let template = match v["template"].as_str() {
        Some(t) => t.to_string(),
        None => return ("error: JSON skill missing 'template' field".into(), true),
    };
    (render_template(&template, args), false)
}

// ── Phase B: Python script skills ────────────────────────────────────────────

/// Parse metadata from a `.py` skill file.
///
/// Metadata is encoded as leading `# key: value` comment lines:
/// ```python
/// # name: process_data
/// # description: Processes structured data and returns a summary
/// # when_to_use: data processing, analysis, summarization
/// ```
pub fn parse_python_skill(path: &Path) -> Option<SkillMetadata> {
    let content = std::fs::read_to_string(path).ok()?;
    let name = extract_py_meta(&content, "name")?;
    let description = extract_py_meta(&content, "description").unwrap_or_default();
    let mut meta = SkillMetadata::new(name, description);
    if let Some(w) = extract_py_meta(&content, "when_to_use") {
        meta = meta.with_when_to_use(w);
    }
    // P1-B: `# allowed_tools: a, b` comment → declared tool ids for skill gating.
    if let Some(t) = extract_py_meta(&content, "allowed_tools") {
        meta.allowed_tools = t.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).map(Into::into).collect();
    }
    Some(meta)
}

/// Resource limits for Python skill subprocess execution.
pub struct PythonSkillPolicy {
    /// Hard timeout for the subprocess. Default: 30 seconds.
    pub timeout_ms: u64,
    /// Maximum combined stdout+stderr size before truncation. Default: 64 KiB.
    pub max_output_bytes: usize,
}

impl Default for PythonSkillPolicy {
    fn default() -> Self {
        Self {
            timeout_ms: 30_000,
            max_output_bytes: 65_536,
        }
    }
}

/// Execute a `.py` skill file via a sandboxed Python subprocess.
///
/// Each invocation runs in an isolated temporary directory (unique per call)
/// so concurrent skill calls cannot interfere. Args are JSON-encoded and passed
/// via the `SKILL_ARGS` environment variable. The combined stdout+stderr is
/// returned as the skill result.
///
/// The subprocess is killed and an error is returned if it exceeds
/// `policy.timeout_ms`.
pub async fn execute_python_skill(
    path: &Path,
    args: &HashMap<String, serde_json::Value>,
    sandbox_base: Option<&Path>,
    policy: &PythonSkillPolicy,
) -> (String, bool) {
    let script = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return (format!("error: could not read skill file: {e}"), true),
    };
    let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".into());

    // Unique workdir per invocation — prevents races between concurrent calls.
    let base = sandbox_base
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("deepstrike-skills"));
    let invocation_id = uuid::Uuid::new_v4().to_string();
    let work_dir = base.join(invocation_id);

    if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
        return (format!("error: cannot create sandbox dir: {e}"), true);
    }

    let mut child = match tokio::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .env_clear()
        .env("SKILL_ARGS", &args_json)
        .env("HOME", work_dir.to_string_lossy().as_ref())
        .env("TMPDIR", work_dir.to_string_lossy().as_ref())
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .current_dir(&work_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&work_dir).await;
            return (format!("error: failed to spawn python3: {e}"), true);
        }
    };

    // Drain stdout/stderr concurrently before waiting so the child never
    // blocks on a full pipe buffer (same pattern as ProcessSandboxPlane).
    use tokio::io::AsyncReadExt;
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let read_out = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::BufReader::new(stdout_pipe).read_to_end(&mut buf).await.ok();
        buf
    });
    let read_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        tokio::io::BufReader::new(stderr_pipe).read_to_end(&mut buf).await.ok();
        buf
    });

    let timeout_dur = tokio::time::Duration::from_millis(policy.timeout_ms);
    let timed_out = tokio::time::timeout(timeout_dur, child.wait()).await;

    // Best-effort cleanup of the per-invocation workdir.
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let is_error = match timed_out {
        Ok(Ok(status)) => !status.success(),
        Ok(Err(e)) => {
            read_out.abort();
            read_err.abort();
            return (format!("error: subprocess IO error: {e}"), true);
        }
        Err(_) => {
            child.kill().await.ok();
            child.wait().await.ok();
            read_out.abort();
            read_err.abort();
            return (
                format!("error: skill timed out after {}ms", policy.timeout_ms),
                true,
            );
        }
    };

    let out_bytes = read_out.await.unwrap_or_default();
    let err_bytes = read_err.await.unwrap_or_default();
    let mut combined = [out_bytes, err_bytes].concat();
    if combined.len() > policy.max_output_bytes {
        combined.truncate(policy.max_output_bytes);
        combined.extend_from_slice(b"\n[output truncated]");
    }
    let text = String::from_utf8_lossy(&combined).into_owned();
    (if text.is_empty() { "(no output)".into() } else { text }, is_error)
}

// ── Template engine ───────────────────────────────────────────────────────────

fn render_template(template: &str, args: &HashMap<String, serde_json::Value>) -> String {
    let mut out = template.to_string();
    for (k, v) in args {
        let placeholder = format!("{{{{{k}}}}}");
        let val = match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out = out.replace(&placeholder, &val);
    }
    out
}

// ── Python metadata helper ────────────────────────────────────────────────────

fn extract_py_meta(content: &str, key: &str) -> Option<String> {
    let prefix = format!("# {key}:");
    content
        .lines()
        .find(|l| l.trim_start().starts_with(&prefix))
        .map(|l| {
            let pos = l.find(&prefix).unwrap() + prefix.len();
            l[pos..].trim().to_string()
        })
}

// ── Scan helpers ──────────────────────────────────────────────────────────────

/// Scan a skill directory and return metadata for all recognised skill files.
///
/// Priority per name: `.md` > `.json` > `.py`. If multiple extensions exist
/// for the same base name, only the highest-priority one is returned.
pub fn scan_skill_dir(dir: &Path) -> Vec<SkillMetadata> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    // Collect by base name so we deduplicate if .md and .json both exist.
    let mut by_name: HashMap<String, (u8, SkillMetadata)> = HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(kind) = SkillKind::from_path(&path) else {
            continue;
        };
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            continue;
        }

        let priority = match kind {
            SkillKind::Prompt => 0u8,
            SkillKind::ComputeJson => 1,
            SkillKind::PythonScript => 2,
        };

        let meta = match kind {
            SkillKind::Prompt => {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let mut meta = SkillMetadata::new(name.clone(), parse_md_description(&content));
                    // P1-B: `allowed_tools: a, b` (or `[a, b]`) frontmatter line → declared tool ids.
                    if let Some(line) =
                        content.lines().find(|l| l.trim_start().starts_with("allowed_tools:"))
                    {
                        let raw = line.splitn(2, ':').nth(1).unwrap_or("");
                        meta.allowed_tools = raw
                            .trim()
                            .trim_matches(|c| c == '[' || c == ']')
                            .split(',')
                            .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\''))
                            .filter(|s| !s.is_empty())
                            .map(Into::into)
                            .collect();
                    }
                    Some(meta)
                } else {
                    None
                }
            }
            SkillKind::ComputeJson => parse_json_skill(&path),
            SkillKind::PythonScript => parse_python_skill(&path),
        };

        if let Some(meta) = meta {
            let entry = by_name.entry(name).or_insert((255, meta.clone()));
            if priority < entry.0 {
                *entry = (priority, meta);
            }
        }
    }

    by_name.into_values().map(|(_, m)| m).collect()
}

fn parse_md_description(content: &str) -> String {
    let body = content.trim_start();
    if !body.starts_with("---") {
        return String::new();
    }
    let rest = &body[3..];
    let end = rest.find("\n---").unwrap_or(rest.len());
    for line in rest[..end].lines() {
        if let Some(val) = line.strip_prefix("description:") {
            return val.trim().to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_kind_from_extension() {
        assert_eq!(SkillKind::from_path(Path::new("a.md")), Some(SkillKind::Prompt));
        assert_eq!(SkillKind::from_path(Path::new("b.json")), Some(SkillKind::ComputeJson));
        assert_eq!(SkillKind::from_path(Path::new("c.py")), Some(SkillKind::PythonScript));
        assert_eq!(SkillKind::from_path(Path::new("d.ts")), None);
        assert_eq!(SkillKind::from_path(Path::new("noext")), None);
    }

    #[test]
    fn template_substitutes_string_and_numeric() {
        let mut args = HashMap::new();
        args.insert("name".into(), serde_json::Value::String("Alice".into()));
        args.insert("count".into(), serde_json::json!(42));
        let result = render_template("Hello, {{name}}! You have {{count}} items.", &args);
        assert_eq!(result, "Hello, Alice! You have 42 items.");
    }

    #[test]
    fn template_leaves_unknown_placeholders() {
        let args = HashMap::new();
        let result = render_template("Hi {{name}}!", &args);
        assert_eq!(result, "Hi {{name}}!");
    }

    #[test]
    fn extract_py_meta_parses_comment_lines() {
        let src = "# name: my_skill\n# description: Does stuff\nimport os\n";
        assert_eq!(extract_py_meta(src, "name").as_deref(), Some("my_skill"));
        assert_eq!(extract_py_meta(src, "description").as_deref(), Some("Does stuff"));
        assert_eq!(extract_py_meta(src, "missing"), None);
    }

    #[test]
    fn parse_md_description_extracts_frontmatter() {
        let md = "---\ndescription: A useful skill\nauthor: test\n---\n# Heading\n";
        assert_eq!(parse_md_description(md), "A useful skill");
    }

    #[test]
    fn parse_md_description_empty_without_frontmatter() {
        assert_eq!(parse_md_description("# No frontmatter"), "");
    }
}
