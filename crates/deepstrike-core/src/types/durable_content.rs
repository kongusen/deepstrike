//! Versioned provider-neutral content used by durable events/checkpoints.
//! Protocol adapters own vendor serialization; this DTO owns only portable content and payload
//! locator semantics. Unknown fields and nested tool results fail closed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableContent {
    pub schema_version: u32,
    pub blocks: Vec<DurableContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableToolResult {
    pub schema_version: u32,
    pub call_id: String,
    pub is_error: bool,
    pub blocks: Vec<DurableContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableContentBlock {
    Text {
        text: String,
    },
    Image {
        source: DurableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options: Option<serde_json::Value>,
    },
    Audio {
        source: DurableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options: Option<serde_json::Value>,
    },
    Video {
        source: DurableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options: Option<serde_json::Value>,
    },
    File {
        source: DurableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_options: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableSource {
    Url {
        url: String,
    },
    Base64 {
        data: String,
    },
    FileId {
        id: String,
        affinity: EndpointAffinity,
    },
    Object {
        handle: String,
        owner: String,
        payload_ref: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointAffinity {
    pub provider_id: String,
    pub endpoint_id: String,
}

impl DurableContent {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    pub fn text(text: impl Into<String>) -> Self {
        Self {
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            blocks: vec![DurableContentBlock::Text { text: text.into() }],
        }
    }

    pub fn decode(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        let content: Self = serde_json::from_value(value)?;
        content.validate()?;
        Ok(content)
    }

    pub fn validate(&self) -> Result<(), serde_json::Error> {
        if self.schema_version != Self::CURRENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported durable content schema_version {}",
                self.schema_version
            )));
        }
        for block in &self.blocks {
            validate_block(block)?;
        }
        Ok(())
    }
}

impl DurableToolResult {
    pub const CURRENT_SCHEMA_VERSION: u32 = 1;

    pub fn legacy_text(
        call_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            schema_version: Self::CURRENT_SCHEMA_VERSION,
            call_id: call_id.into(),
            is_error,
            blocks: vec![DurableContentBlock::Text {
                text: output.into(),
            }],
        }
    }

    pub fn decode(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        if value.get("schema_version").is_none() && value.get("output").is_some() {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Legacy {
                call_id: String,
                output: String,
                #[serde(default)]
                is_error: bool,
            }
            let legacy: Legacy = serde_json::from_value(value)?;
            return Ok(Self::legacy_text(
                legacy.call_id,
                legacy.output,
                legacy.is_error,
            ));
        }
        let result: Self = serde_json::from_value(value)?;
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), serde_json::Error> {
        if self.schema_version != Self::CURRENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "unsupported durable tool result schema_version {}",
                self.schema_version
            )));
        }
        require_non_empty(&self.call_id, "tool result call_id")?;
        for block in &self.blocks {
            validate_block(block)?;
        }
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> serde_json::Error {
    serde_json::Error::io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

fn require_non_empty(value: &str, label: &str) -> Result<(), serde_json::Error> {
    if value.is_empty() {
        return Err(invalid(format!("{label} must be a non-empty string")));
    }
    Ok(())
}

fn validate_block(block: &DurableContentBlock) -> Result<(), serde_json::Error> {
    match block {
        DurableContentBlock::Text { .. } => Ok(()),
        DurableContentBlock::Image {
            source, media_type, ..
        }
        | DurableContentBlock::Audio {
            source, media_type, ..
        }
        | DurableContentBlock::Video {
            source, media_type, ..
        }
        | DurableContentBlock::File {
            source, media_type, ..
        } => {
            validate_source(source)?;
            if let Some(media_type) = media_type {
                require_non_empty(media_type, "content media_type")?;
            }
            Ok(())
        }
    }
}

fn validate_source(source: &DurableSource) -> Result<(), serde_json::Error> {
    match source {
        DurableSource::Url { url } => require_non_empty(url, "content URL"),
        DurableSource::Base64 { data } => {
            require_non_empty(data, "content base64 data")?;
            if !valid_standard_base64(data) {
                return Err(invalid("content base64 data is not valid base64"));
            }
            Ok(())
        }
        DurableSource::FileId { id, affinity } => {
            require_non_empty(id, "content file id")?;
            require_non_empty(&affinity.provider_id, "content file affinity provider_id")?;
            require_non_empty(&affinity.endpoint_id, "content file affinity endpoint_id")
        }
        DurableSource::Object {
            handle,
            owner,
            payload_ref,
        } => {
            require_non_empty(handle, "content object handle")?;
            require_non_empty(owner, "content object owner")?;
            require_non_empty(payload_ref, "content object payload_ref")
        }
    }
}

fn valid_standard_base64(data: &str) -> bool {
    let bytes = data.as_bytes();
    if bytes.len() % 4 != 0 {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|&&byte| byte == b'=').count();
    if padding > 2 || padding > bytes.len() {
        return false;
    }
    bytes[..bytes.len().saturating_sub(padding)]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'+' || *byte == b'/')
        && bytes[bytes.len().saturating_sub(padding)..]
            .iter()
            .all(|byte| *byte == b'=')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_tool_result_migrates_to_v1_text_block() {
        let result =
            DurableToolResult::decode(json!({"call_id":"c1","output":"ok","is_error":false}))
                .unwrap();
        assert_eq!(result, DurableToolResult::legacy_text("c1", "ok", false));
    }

    #[test]
    fn unknown_and_nested_blocks_are_rejected() {
        assert!(
            DurableContent::decode(
                json!({"schema_version":1,"blocks":[{"type":"text","text":"x","extra":true}]})
            )
            .is_err()
        );
        assert!(
            DurableContent::decode(
                json!({"schema_version":1,"blocks":[{"type":"tool_result","call_id":"nested"}]})
            )
            .is_err()
        );
        assert!(DurableContent::decode(json!({"schema_version":1,"blocks":[{"type":"file","source":{"kind":"object","handle":"h","owner":"host"}}]})).is_err());
        assert!(DurableContent::decode(json!({"schema_version":2,"blocks":[]})).is_err());
        assert!(DurableContent::decode(json!({"schema_version":1,"blocks":[{"type":"file","source":{"kind":"file_id","id":"f"}}]})).is_err());
    }

    #[test]
    fn media_affinity_and_payload_ownership_round_trip() {
        let content = DurableContent {
            schema_version: 1,
            blocks: vec![
                DurableContentBlock::File {
                    source: DurableSource::FileId {
                        id: "f1".into(),
                        affinity: EndpointAffinity {
                            provider_id: "openai".into(),
                            endpoint_id: "responses".into(),
                        },
                    },
                    media_type: Some("application/pdf".into()),
                    provider_options: None,
                },
                DurableContentBlock::Video {
                    source: DurableSource::Object {
                        handle: "h1".into(),
                        owner: "host".into(),
                        payload_ref: "sha256:abc".into(),
                    },
                    media_type: Some("video/mp4".into()),
                    provider_options: None,
                },
            ],
        };
        let bytes = serde_json::to_value(&content).unwrap();
        assert_eq!(DurableContent::decode(bytes).unwrap(), content);
    }

    #[test]
    fn shared_v1_tool_result_fixture_round_trips() {
        let fixture =
            include_str!("../../../../tests/fixtures/durable-content/v1-tool-result.json");
        let value: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let decoded = DurableToolResult::decode(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}
