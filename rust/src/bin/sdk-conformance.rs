//! SPC-017 Rust SDK adapter.
//!
//! The differential runner owns fixture validation and comparison. This binary only projects
//! shared fixture inputs through public Rust SDK/Core types and emits one JSON envelope.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use deepstrike_core::runtime::kernel::wire::ProviderStopReason;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::agent::{
    AgentCapabilityFilter, AgentIdentity, AgentRole, AgentRunSpec,
};
use deepstrike_core::types::capability::{CapabilityDescriptor, CapabilityKind};
use deepstrike_core::types::durable_content::{
    DurableContent, DurableContentBlock, DurableToolResult,
};
use deepstrike_core::types::message::{Content, ToolResult};
use deepstrike_sdk::{ProviderRequestEndpoint, ProviderRequestPlan, RecordedPromptMeasurement};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    domain: String,
    input: Value,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expected {
    contract_version: u32,
}

#[derive(Debug)]
struct AdapterError {
    code: &'static str,
    path: &'static str,
    message: String,
}

impl AdapterError {
    fn unsupported_domain(domain: &str) -> Self {
        Self {
            code: "unsupported_domain",
            path: "/domain",
            message: format!("Rust SDK does not expose the {domain} contract"),
        }
    }

    fn failure(message: impl Into<String>) -> Self {
        Self {
            code: "adapter_failure",
            path: "",
            message: message.into(),
        }
    }
}

fn main() {
    let path = match fixture_path_from_args(env::args().skip(1)) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let fixture = match load_fixture(&path) {
        Ok(fixture) => fixture,
        Err(error) => {
            eprintln!("failed to read fixture: {error}");
            std::process::exit(2);
        }
    };

    let envelope = match project(&fixture) {
        Ok(canonical) => json!({
            "ok": true,
            "sdk": "rust",
            "fixture": fixture.id,
            "contractVersion": fixture.expected.contract_version,
            "canonical": canonical,
        }),
        Err(error) => json!({
            "ok": false,
            "sdk": "rust",
            "fixture": fixture.id,
            "contractVersion": fixture.expected.contract_version,
            "error": {
                "code": error.code,
                "path": error.path,
                "message": error.message,
            },
        }),
    };
    println!(
        "{}",
        serde_json::to_string(&envelope).expect("envelope serializes")
    );
}

fn fixture_path_from_args(arguments: impl IntoIterator<Item = String>) -> Result<PathBuf, String> {
    let paths: Vec<String> = arguments.into_iter().collect();
    if paths.len() != 1 {
        return Err("usage: sdk-conformance <absolute-fixture-path>".into());
    }
    let path = PathBuf::from(&paths[0]);
    if !path.is_absolute() {
        return Err("usage: sdk-conformance <absolute-fixture-path>".into());
    }
    Ok(path)
}

fn load_fixture(path: &Path) -> Result<Fixture, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

fn project(fixture: &Fixture) -> Result<Value, AdapterError> {
    match fixture.domain.as_str() {
        "agent_ir" => project_agent_ir(&fixture.input),
        "provider_request_plan" => project_request_plan(&fixture.input),
        "durable_tool_result" => project_durable_tool_result(&fixture.input),
        "prompt_measurement" => project_prompt_measurement(&fixture.input),
        "provider_error" => project_provider_error(&fixture.input),
        "session_event" => project_session_event(&fixture.input),
        domain => Err(AdapterError::unsupported_domain(domain)),
    }
}

fn project_agent_ir(input: &Value) -> Result<Value, AdapterError> {
    let source = read_referenced_fixture(required_value_str(input, "fixture", "/input/fixture")?)?;
    let source = source
        .as_object()
        .ok_or_else(|| AdapterError::failure("agent declaration must be an object"))?;
    let name = required_map_str(source, "name", "/input/fixture/name")?;
    let raw_filter = source
        .get("capabilityFilter")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let raw_filter = raw_filter
        .as_object()
        .ok_or_else(|| AdapterError::failure("agent capabilityFilter must be an object"))?;
    let filter: AgentCapabilityFilter = serde_json::from_value(json!({
        "allowed_kinds": raw_filter.get("allowedKinds").cloned().unwrap_or_else(|| json!([])),
        "allowed_ids": raw_filter.get("allowedIds").cloned().unwrap_or_else(|| json!([])),
    }))
    .map_err(|error| AdapterError::failure(error.to_string()))?;
    let run_spec = AgentRunSpec::new(
        AgentIdentity::new("spc-017", "conformance"),
        AgentRole::Custom,
        name,
    )
    .with_capability_filter(filter.clone());

    let mut effective = Vec::new();
    for candidate in agent_capabilities(source)? {
        let descriptor = CapabilityDescriptor::marker(
            candidate.kind,
            candidate.id.clone(),
            candidate.description.clone(),
        );
        if run_spec.capability_filter.allows(&descriptor) {
            effective.push(json!({
                "kind": capability_kind_name(candidate.kind),
                "id": candidate.id,
                "description": candidate.description,
            }));
        }
    }

    Ok(json!({
        "version": 1,
        "name": name,
        "capabilityFilter": {
            "allowedKinds": filter.allowed_kinds.iter().copied().map(capability_kind_name).collect::<Vec<_>>(),
            "allowedIds": filter.allowed_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
        },
        "effectiveCapabilities": effective,
    }))
}

#[derive(Debug)]
struct AgentCapability {
    kind: CapabilityKind,
    id: String,
    description: String,
}

fn agent_capabilities(
    source: &serde_json::Map<String, Value>,
) -> Result<Vec<AgentCapability>, AdapterError> {
    let mut capabilities = Vec::new();
    for tool in object_array(source, "tools")? {
        capabilities.push(AgentCapability {
            kind: CapabilityKind::Tool,
            id: required_map_str(tool, "name", "/input/fixture/tools/name")?.to_string(),
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    for server in object_array(source, "mcpServers")? {
        let id = server
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                server
                    .get("transport")
                    .and_then(Value::as_object)
                    .and_then(|transport| transport.get("kind"))
                    .and_then(Value::as_str)
            })
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AdapterError::failure("MCP server requires name or transport kind"))?;
        capabilities.push(AgentCapability {
            kind: CapabilityKind::McpServer,
            id: id.to_string(),
            description: id.to_string(),
        });
    }
    for skill in object_array(source, "skills")? {
        capabilities.push(AgentCapability {
            kind: CapabilityKind::Skill,
            id: required_map_str(skill, "name", "/input/fixture/skills/name")?.to_string(),
            description: skill
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    Ok(capabilities)
}

fn object_array<'a>(
    source: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a serde_json::Map<String, Value>>, AdapterError> {
    source
        .get(key)
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| AdapterError::failure(format!("{key} must be an array")))?
                .iter()
                .map(|value| {
                    value.as_object().ok_or_else(|| {
                        AdapterError::failure(format!("{key} items must be objects"))
                    })
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn capability_kind_name(kind: CapabilityKind) -> &'static str {
    match kind {
        CapabilityKind::Tool => "tool",
        CapabilityKind::Skill => "skill",
        CapabilityKind::Memory => "memory",
        CapabilityKind::Knowledge => "knowledge",
        CapabilityKind::McpServer => "mcp_server",
        CapabilityKind::Command => "command",
        CapabilityKind::Agent => "agent",
    }
}

fn project_request_plan(input: &Value) -> Result<Value, AdapterError> {
    let reference = required_value_str(input, "fixture", "/input/fixture")?;
    let request = read_referenced_fixture(reference)?;
    let request_input = required_object(&request, "input", "/input")?;
    let endpoint = required_map_object(request_input, "endpoint", "/input/endpoint")?;
    let tools = request_input
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| AdapterError::failure("request plan tools must be an array"))?
        .clone();
    let plan = ProviderRequestPlan::new(
        required_map_str(request_input, "providerId", "/input/providerId")?,
        required_map_str(request_input, "modelId", "/input/modelId")?,
        ProviderRequestEndpoint::new(
            required_map_str(endpoint, "id", "/input/endpoint/id")?,
            required_map_str(endpoint, "protocol", "/input/endpoint/protocol")?,
            required_map_str(endpoint, "baseURL", "/input/endpoint/baseURL")?,
        ),
        request_input
            .get("context")
            .cloned()
            .ok_or_else(|| AdapterError::failure("request plan context is required"))?,
        tools,
        request_input
            .get("options")
            .cloned()
            .ok_or_else(|| AdapterError::failure("request plan options are required"))?,
    )
    .map_err(|error| AdapterError::failure(error.to_string()))?;
    Ok(json!({ "fingerprint": plan.fingerprint }))
}

fn project_durable_tool_result(input: &Value) -> Result<Value, AdapterError> {
    let value = match input.get("fixture").and_then(Value::as_str) {
        Some(reference) => read_referenced_fixture(reference)?,
        None => input
            .get("value")
            .cloned()
            .ok_or_else(|| AdapterError::failure("durable tool result value is required"))?,
    };
    let result = DurableToolResult::decode(value).map_err(|error| AdapterError {
        code: "invalid_durable_tool_result",
        path: "/is_error",
        message: error.to_string(),
    })?;
    Ok(json!({
        "schema_version": result.schema_version,
        "call_id": result.call_id,
        "is_error": result.is_error,
        "blockTypes": result.blocks.iter().map(block_type).collect::<Vec<_>>(),
    }))
}

fn project_prompt_measurement(input: &Value) -> Result<Value, AdapterError> {
    let value = input
        .get("value")
        .cloned()
        .ok_or_else(|| AdapterError::failure("prompt measurement value is required"))?;
    let measurement: RecordedPromptMeasurement =
        serde_json::from_value(value).map_err(|error| AdapterError {
            code: "invalid_prompt_measurement",
            path: "/input/value",
            message: error.to_string(),
        })?;
    serde_json::to_value(measurement).map_err(|error| AdapterError::failure(error.to_string()))
}

fn project_provider_error(input: &Value) -> Result<Value, AdapterError> {
    let stop_reason = required_value_str(input, "stopReason", "/stopReason")?;
    let decoded = serde_json::from_value::<ProviderStopReason>(Value::String(stop_reason.into()));
    match decoded {
        Ok(reason) => Ok(json!({ "stopReason": reason.as_str() })),
        Err(error) => Err(AdapterError {
            code: "unknown_stop_reason",
            path: "/stopReason",
            message: error.to_string(),
        }),
    }
}

fn project_session_event(input: &Value) -> Result<Value, AdapterError> {
    let event = input
        .get("event")
        .and_then(Value::as_object)
        .ok_or_else(|| AdapterError::failure("session event is required"))?;
    if event.get("kind").and_then(Value::as_str) != Some("tool_completed") {
        return Err(AdapterError::unsupported_domain("session_event kind"));
    }
    let call_id = required_map_str(event, "callId", "/input/event/callId")?;
    let is_error = event
        .get("isError")
        .and_then(Value::as_bool)
        .ok_or_else(|| AdapterError::failure("session event isError must be a boolean"))?;
    let content = event
        .get("content")
        .cloned()
        .ok_or_else(|| AdapterError::failure("session event content is required"))?;
    let durable_content = DurableContent::decode(content).map_err(|error| AdapterError {
        code: "invalid_session_event",
        path: "/input/event/content",
        message: error.to_string(),
    })?;

    // Construct the actual core event before projecting it. The event remains durable through
    // the ToolResult carrier; the adapter exposes only the stable conformance fields.
    let event = SessionEvent::ToolCompleted {
        turn: 0,
        results: vec![ToolResult {
            call_id: call_id.into(),
            output: Content::Text(String::new()),
            durable_content: Some(durable_content),
            is_error,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }],
    };
    let SessionEvent::ToolCompleted { results, .. } = event else {
        unreachable!("constructed tool completion event")
    };
    let result = results.first().expect("constructed one tool result");
    let content = result
        .durable_content
        .as_ref()
        .expect("constructed durable content");
    Ok(json!({
        "kind": "tool_completed",
        "callId": result.call_id,
        "isError": result.is_error,
        "blockTypes": content.blocks.iter().map(block_type).collect::<Vec<_>>(),
    }))
}

fn read_referenced_fixture(reference: &str) -> Result<Value, AdapterError> {
    let relative = Path::new(reference);
    if reference.is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_fixture_reference(
            "fixture reference must be a relative path under tests/fixtures",
        ));
    }
    let fixtures_root = fs::canonicalize(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures"),
    )
    .map_err(|error| AdapterError::failure(error.to_string()))?;
    let path = fs::canonicalize(fixtures_root.join(relative)).map_err(|_| {
        invalid_fixture_reference("fixture reference must resolve under tests/fixtures")
    })?;
    if path == fixtures_root || !path.starts_with(&fixtures_root) {
        return Err(invalid_fixture_reference(
            "fixture reference must stay under tests/fixtures",
        ));
    }
    if !path.is_file() {
        return Err(invalid_fixture_reference("fixture reference must be a file"));
    }
    let raw = fs::read_to_string(path).map_err(|error| AdapterError::failure(error.to_string()))?;
    serde_json::from_str(&raw).map_err(|error| AdapterError::failure(error.to_string()))
}

fn invalid_fixture_reference(message: impl Into<String>) -> AdapterError {
    AdapterError {
        code: "invalid_fixture_reference",
        path: "/input/fixture",
        message: message.into(),
    }
}

fn required_object<'a>(
    value: &'a Value,
    key: &str,
    path: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, AdapterError> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| AdapterError {
            code: "adapter_failure",
            path,
            message: format!("{key} must be an object"),
        })
}

fn required_map_object<'a>(
    value: &'a serde_json::Map<String, Value>,
    key: &str,
    path: &'static str,
) -> Result<&'a serde_json::Map<String, Value>, AdapterError> {
    value
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| AdapterError {
            code: "adapter_failure",
            path,
            message: format!("{key} must be an object"),
        })
}

fn required_map_str<'a>(
    value: &'a serde_json::Map<String, Value>,
    key: &str,
    path: &'static str,
) -> Result<&'a str, AdapterError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AdapterError {
            code: "adapter_failure",
            path,
            message: format!("{key} must be a non-empty string"),
        })
}

fn required_value_str<'a>(
    value: &'a Value,
    key: &str,
    path: &'static str,
) -> Result<&'a str, AdapterError> {
    value
        .as_object()
        .ok_or_else(|| AdapterError::failure(format!("{key} must be an object")))
        .and_then(|object| required_map_str(object, key, path))
}

fn block_type(block: &DurableContentBlock) -> &'static str {
    match block {
        DurableContentBlock::Text { .. } => "text",
        DurableContentBlock::Image { .. } => "image",
        DurableContentBlock::Audio { .. } => "audio",
        DurableContentBlock::Video { .. } => "video",
        DurableContentBlock::File { .. } => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Fixture {
        let raw = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/sdk-conformance/v1")
                .join(format!("{name}.json")),
        )
        .expect("fixture reads");
        serde_json::from_str(&raw).expect("fixture decodes")
    }

    #[test]
    fn projects_rust_representable_contracts() {
        let plan = project(&fixture("provider-request-plan-v1")).expect("request plan projects");
        assert_eq!(
            plan["fingerprint"],
            "sha256:d91b9737b9a80295599b8f3804a0168c11de2f4ae1de608bfcac824cdf31b8d2"
        );

        let durable = project(&fixture("durable-tool-result-v1")).expect("durable result projects");
        assert_eq!(
            durable["blockTypes"],
            json!(["text", "image", "file", "video"])
        );

        let measurement = project(&fixture("prompt-measurement-v1")).expect("measurement projects");
        assert_eq!(measurement["inputTokens"], 12);

        let event =
            project(&fixture("session-event-tool-completed")).expect("session event projects");
        assert_eq!(event["blockTypes"], json!(["text"]));
    }

    #[test]
    fn emits_stable_structured_errors_for_invalid_or_unknown_contract_values() {
        let durable = project(&fixture("durable-tool-result-invalid-is-error"))
            .expect_err("invalid bool rejects");
        assert_eq!(durable.code, "invalid_durable_tool_result");
        assert_eq!(durable.path, "/is_error");

        let stop =
            project(&fixture("provider-error-unknown-stop")).expect_err("unknown stop rejects");
        assert_eq!(stop.code, "unknown_stop_reason");
        assert_eq!(stop.path, "/stopReason");
    }

    #[test]
    fn projects_agent_ir_through_the_rust_agent_capability_contract() {
        let ir = project(&fixture("agent-ir-basic")).expect("agent IR projects");
        assert_eq!(ir["name"], "researcher");
        assert_eq!(
            ir["effectiveCapabilities"],
            json!([
                { "kind": "tool", "id": "web_search", "description": "Search the web for source material." },
                { "kind": "skill", "id": "citations", "description": "Citation policy." },
            ])
        );
    }

    #[test]
    fn command_line_requires_exactly_one_absolute_fixture_path() {
        assert!(fixture_path_from_args(Vec::<String>::new()).is_err());
        assert!(fixture_path_from_args(vec!["relative.json".into()]).is_err());
        assert!(fixture_path_from_args(vec!["/tmp/fixture.json".into(), "extra".into()]).is_err());
        assert_eq!(
            fixture_path_from_args(vec!["/tmp/fixture.json".into()]).unwrap(),
            PathBuf::from("/tmp/fixture.json"),
        );
    }

    #[test]
    fn fixture_reference_must_resolve_under_the_fixture_root() {
        for reference in ["", ".", "agent-ir/../agent-ir/v1-agent.json", "/tmp/agent.json"] {
            let error = read_referenced_fixture(reference).expect_err("reference must reject");
            assert_eq!(error.code, "invalid_fixture_reference");
            assert_eq!(error.path, "/input/fixture");
        }
    }

    #[cfg(unix)]
    #[test]
    fn fixture_reference_rejects_a_symlink_escaping_the_fixture_root() {
        use std::os::unix::fs::symlink;

        let outside = std::env::temp_dir().join(format!(
            "deepstrike-sdk-conformance-{}-agent.json",
            std::process::id()
        ));
        let link = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures")
            .join(format!(".sdk-conformance-escape-{}.json", std::process::id()));
        fs::write(&outside, "{}\n").expect("outside fixture writes");
        symlink(&outside, &link).expect("symlink creates");
        let result = read_referenced_fixture(link.file_name().unwrap().to_str().unwrap());
        fs::remove_file(&link).expect("symlink removes");
        fs::remove_file(&outside).expect("outside fixture removes");

        let error = result.expect_err("symlink escape must reject");
        assert_eq!(error.code, "invalid_fixture_reference");
        assert_eq!(error.path, "/input/fixture");
    }
}
