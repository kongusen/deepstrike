//! SPC-017 Rust SDK adapter.
//!
//! The differential runner owns fixture validation and comparison. This binary only projects
//! shared fixture inputs through public Rust SDK/Core types and emits one JSON envelope.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use deepstrike_core::runtime::kernel::wire::ProviderStopReason;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::durable_content::{
    DurableContent, DurableContentBlock, DurableToolResult,
};
use deepstrike_core::types::message::{Content, ToolResult};
use deepstrike_sdk::runtime::project_shared_evidence;
use deepstrike_sdk::runtime::read_shared_evidence_values;
use deepstrike_sdk::{ProviderRequestEndpoint, ProviderRequestPlan, RecordedPromptMeasurement};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    domain: String,
    input: Value,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    #[serde(default)]
    canonical: Option<Value>,
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
            "canonical": canonical,
        }),
        Err(error) => json!({
            "ok": false,
            "sdk": "rust",
            "fixture": fixture.id,
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
        "context_execution" => {
            let request: deepstrike_core::context::execution::ContextDispatchRequest =
                serde_json::from_value(fixture.input["request"].clone()).map_err(|error| {
                    AdapterError {
                        code: "invalid_context".into(),
                        path: "/input/request".into(),
                        message: error.to_string(),
                    }
                })?;
            let prepared = deepstrike_core::context::execution::prepare_context_dispatch(&request)
                .map_err(|error| AdapterError {
                    code: "invalid_context".into(),
                    path: "/input/request".into(),
                    message: error.to_string(),
                })?;
            deepstrike_core::context::execution::verify_context_dispatch(
                &request.effect,
                &prepared,
            )
            .map_err(|error| AdapterError {
                code: "invalid_context".into(),
                path: "/input/request".into(),
                message: error.to_string(),
            })?;
            Ok(
                json!({ "input_digest": prepared.execution_input.input_digest,
                "plan_digest": prepared.plan.plan_id, "verified": true }),
            )
        }
        "provider_request_plan" => project_request_plan(&fixture.input),
        "durable_tool_result" => project_durable_tool_result(&fixture.input),
        "prompt_measurement" => project_prompt_measurement(&fixture.input),
        "provider_error" => project_provider_error(&fixture.input),
        "session_event" => project_session_event(&fixture.input),
        "session_event_vocabulary" => project_session_event_vocabulary(&fixture.input),
        "shared_evidence_snapshot" => project_shared_evidence_snapshot(&fixture.input),
        "kernel_observation_projection" => project_kernel_observations(&fixture.input),
        domain => Err(AdapterError::unsupported_domain(domain)),
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
    let plan = match request_input.get("execution") {
        Some(execution) => plan.with_execution(execution.clone()),
        None => plan,
    };
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
        }],
        effect_id: None,
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

/// The kinds `SessionEvent` carries, each with the top-level fields its serde shape treats as
/// required/optional — probed on the shared sample rather than read from the registry: a field is
/// optional when the sample still decodes without it and the re-encoded event then omits it.
fn project_session_event_vocabulary(input: &Value) -> Result<Value, AdapterError> {
    let samples = required_object(input, "samples", "/input/samples")?;
    let mut kinds = serde_json::Map::new();
    for kind in SessionEvent::KINDS {
        let sample = samples.get(*kind).ok_or_else(|| AdapterError {
            code: "missing_sample",
            path: "/input/samples",
            message: format!("no shared sample for Rust kind {kind}"),
        })?;
        let decode = |value: Value| {
            serde_json::from_value::<SessionEvent>(value)
                .and_then(|event| serde_json::to_value(&event))
                .ok()
        };
        let encoded = decode(sample.clone()).ok_or_else(|| AdapterError {
            code: "invalid_session_event",
            path: "/input/samples",
            message: format!("shared {kind} sample does not decode in Rust"),
        })?;
        let (mut required, mut optional) = (Vec::new(), Vec::new());
        for field in encoded.as_object().into_iter().flat_map(|map| map.keys()) {
            if field == "kind" {
                continue;
            }
            let mut without = sample.clone();
            without.as_object_mut().map(|map| map.remove(field));
            match decode(without) {
                Some(event) if event.get(field).is_none() => optional.push(field.clone()),
                _ => required.push(field.clone()),
            }
        }
        required.sort();
        optional.sort();
        kinds.insert(
            kind.to_string(),
            json!({ "required": required, "optional": optional }),
        );
    }
    Ok(json!({ "kinds": kinds }))
}

fn project_shared_evidence_snapshot(input: &Value) -> Result<Value, AdapterError> {
    let cases = input
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| AdapterError::failure("shared_evidence_snapshot input must carry cases"))?;
    let mut snapshots = serde_json::Map::new();
    for item in cases {
        let name = required_value_str(item, "name", "/input/cases/name")?;
        let operation_id =
            required_value_str(item, "operation_id", "/input/cases/operation_id")?.to_owned();
        let session_id =
            required_value_str(item, "session_id", "/input/cases/session_id")?.to_owned();
        let scope = item.get("scope").cloned().unwrap_or_else(|| json!({}));
        let events = item
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::failure("shared evidence events must be an array"))?;
        let journal = item.get("journal");
        let sidecar = item.get("sidecar");
        let event_values = events
            .iter()
            .enumerate()
            .map(|(seq, event)| (seq as u64, event.clone()))
            .collect::<Vec<_>>();
        let snapshot = read_shared_evidence_values(
            &operation_id,
            &session_id,
            &scope,
            &event_values,
            journal,
            sidecar,
        );
        snapshots.insert(name.to_string(), project_shared_evidence(&snapshot));
    }
    Ok(json!({ "snapshots": snapshots }))
}

fn project_kernel_observations(input: &Value) -> Result<Value, AdapterError> {
    use deepstrike_sdk::runtime::kernel_event_log::{
        ProjectionOptions, kernel_observation_to_session_event_value,
    };
    let cases = input
        .get("cases")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AdapterError::failure("kernel_observation_projection input must carry cases")
        })?;
    let mut events = serde_json::Map::new();
    for case in cases {
        let name = required_value_str(case, "name", "/input/cases/name")?;
        let options = case.get("options").cloned().unwrap_or(Value::Null);
        let opts = ProjectionOptions {
            next_archive_start: options["next_archive_start"].as_u64().unwrap_or(0),
            latest_seq: options["latest_seq"].as_u64(),
            preserved_refs: serde_json::from_value(options["preserved_refs"].clone())
                .unwrap_or_default(),
        };
        let turn = case.get("turn").and_then(Value::as_u64).unwrap_or(0) as u32;
        let event = kernel_observation_to_session_event_value(&case["observation"], turn, &opts);
        events.insert(name.to_string(), event.unwrap_or(Value::Null));
    }
    Ok(json!({ "events": events }))
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
    let fixtures_root =
        fs::canonicalize(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures"))
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
        return Err(invalid_fixture_reference(
            "fixture reference must be a file",
        ));
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
                .join("../tests/fixtures/sdk-conformance/canonical")
                .join(format!("{name}.json")),
        )
        .expect("fixture reads");
        serde_json::from_str(&raw).expect("fixture decodes")
    }

    #[test]
    fn projects_rust_representable_contracts() {
        let request_plan_fixture = fixture("provider-request-plan");
        let plan = project(&request_plan_fixture).expect("request plan projects");
        assert_eq!(
            plan,
            request_plan_fixture
                .expected
                .canonical
                .expect("request plan fixture has canonical expectation")
        );

        let durable = project(&fixture("durable-tool-result")).expect("durable result projects");
        assert_eq!(
            durable["blockTypes"],
            json!(["text", "image", "file", "video"])
        );

        let measurement = project(&fixture("prompt-measurement")).expect("measurement projects");
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
        for reference in [
            "",
            ".",
            "provider-request-plan/../provider-request-plan.json",
            "/tmp/agent.json",
        ] {
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
            .join(format!(
                ".sdk-conformance-escape-{}.json",
                std::process::id()
            ));
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
